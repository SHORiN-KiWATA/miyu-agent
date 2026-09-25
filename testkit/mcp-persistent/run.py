#!/usr/bin/env python3
"""走查：MCP 服务器按会话常驻（09-25）。

隔离家目录 + 独立端口 daemon + 专用桩模型（`stub_llm.py`：每轮先调一次 `mcp_counter_count`，
拿到结果原样说出来）+ 有状态的假服务器（`fake_server.py`：count 在进程里计数，结果带 pid）。

    same_session_keeps_state          会话 a 连着两轮：count 1 → count 2，同一个进程
    other_session_starts_fresh        会话 b：count 1，另一个进程
    instructions_in_system_prompt     请求的 system 里有 <mcp-server-instructions> 和服务器那句说明
    delete_session_reaps_its_process  删掉会话 a：它那个进程没了，b 的还在
    daemon_stop_leaves_no_orphans     daemon 收到 SIGTERM 退出之后：一个服务器进程都不剩

用法：python3 testkit/mcp-persistent/run.py [miyu 二进制]   （默认 target/debug/miyu）
"""

import json
import os
import re
import shutil
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def wait_for(predicate, timeout, step=0.2):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return predicate()


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # 已经退出、还没人收的僵尸也算没了。
    try:
        state = Path(f"/proc/{pid}/stat").read_text().split(")")[-1].split()[0]
        return state != "Z"
    except OSError:
        return True


def recv_exact(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise OSError("socket closed")
        data += chunk
    return data


class Sandbox:
    def __init__(self, miyu):
        self.miyu = miyu
        self.port = free_port()
        self.stub_port = free_port()
        self.home = Path(tempfile.mkdtemp(prefix="miyu-mcp-persistent-", dir=str(Path.home() / ".cache")))
        self.run = self.home / "run"
        self.work = self.home / "work"
        for path in (self.run, self.home / "config", self.home / "cache", self.work):
            path.mkdir(parents=True, exist_ok=True)
        # 服务器进程关在会话的沙盒里（默认沙盒开着），标记文件放在它写得进去的缓存目录。
        self.marker = self.home / "cache" / "mcp-starts"
        self.requests = self.home / "requests.jsonl"
        config = {
            "config_version": 3,
            "oobe_done": True,
            "active_provider": "stub",
            "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
            "providers": [{
                "id": "stub", "display_name": "Stub", "enabled": True,
                "base_url": f"http://127.0.0.1:{self.stub_port}/v1", "protocol": "openai-chat",
                "api_key": "stub", "models": ["stub-model"], "default_model": "stub-model",
            }],
            "memory": {"enabled": False},
            "voice": {"enabled": False},
            "mcp": {"enabled": True, "servers": [{
                "id": "counter", "command": sys.executable,
                "args": [str(HERE / "fake_server.py")],
                "env": {"MARKER": str(self.marker)},
                "timeout_seconds": 10,
            }]},
        }
        (self.home / "config" / "config.jsonc").write_text(json.dumps(config), encoding="utf-8")
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith("HERDR_")
                    and k not in ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "MIYU_HOME",
                                  "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME")}
        self.env.update(MIYU_HOME=str(self.home), XDG_RUNTIME_DIR=str(self.run), LANG="zh_CN.UTF-8",
                        MIYU_LOG="info")
        self.stub = None
        self.daemon = None

    def start(self):
        self.stub = subprocess.Popen(
            [sys.executable, str(HERE / "stub_llm.py")],
            env=dict(self.env, STUB_PORT=str(self.stub_port), STUB_REQUEST_LOG=str(self.requests)),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        log = (self.home / "daemon.log").open("w")
        self.daemon = subprocess.Popen([str(self.miyu), "__daemon", "--port", str(self.port)], env=self.env,
                                       cwd=str(self.work), stdin=subprocess.DEVNULL, stdout=log,
                                       stderr=subprocess.STDOUT)
        if not wait_for(self.ping, 40):
            raise SystemExit("daemon 没起来:\n" + (self.home / "daemon.log").read_text()[-2000:])

    def ping(self):
        try:
            return self.ipc({"command": "ping"}) is not None
        except OSError:
            return False

    def ipc(self, command):
        sock_path = next(iter(self.run.rglob("*.sock")), None)
        if sock_path is None:
            raise OSError("no socket yet")
        payload = json.dumps({"version": 3, **command}).encode()
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(20)
            sock.connect(str(sock_path))
            sock.sendall(struct.pack(">I", len(payload)) + payload)
            (length,) = struct.unpack(">I", recv_exact(sock, 4))
            return json.loads(recv_exact(sock, length))

    def ask(self, session, text, create=False):
        args = [str(self.miyu), "ask", "--output-format", "json", "--session", session]
        if create:
            args.append("--create")
        proc = subprocess.run([*args, text], env=self.env, stdin=subprocess.DEVNULL, cwd=str(self.work),
                              capture_output=True, text=True, timeout=120)
        if proc.returncode != 0:
            raise AssertionError(f"ask {session!r} failed ({proc.returncode}): {proc.stderr[-600:]!r}")

    def session_id(self, name):
        for db in sorted(self.home.rglob("conversation.db")):
            con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
            row = con.execute("SELECT session_id FROM sessions WHERE name = ? AND kind = 'user'",
                              (name,)).fetchone()
            con.close()
            if row:
                return row[0]
        return None

    def tool_results(self):
        """每一轮工具结果（桩收到的最后一条是 tool 的那些请求），按先后。"""
        results = []
        if not self.requests.exists():
            return results
        for line in self.requests.read_text(encoding="utf-8").splitlines():
            entry = json.loads(line)
            if entry.get("last_role") == "tool":
                match = re.search(r"count (\d+) pid (\d+)", entry.get("last", ""))
                if match:
                    results.append((int(match.group(1)), int(match.group(2))))
        return results

    def systems(self):
        if not self.requests.exists():
            return []
        return [json.loads(line).get("system", "")
                for line in self.requests.read_text(encoding="utf-8").splitlines()]

    def marker_pids(self):
        if not self.marker.exists():
            return []
        return [int(line) for line in self.marker.read_text().split()]

    def stop_daemon(self):
        if self.daemon and self.daemon.poll() is None:
            self.daemon.send_signal(signal.SIGTERM)
            try:
                self.daemon.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.daemon.kill()

    def close(self):
        self.stop_daemon()
        if self.stub and self.stub.poll() is None:
            self.stub.terminate()
        shutil.rmtree(self.home, ignore_errors=True)


def main():
    miyu = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "target" / "debug" / "miyu").resolve()
    box = Sandbox(miyu)
    report = {}
    try:
        box.start()
        box.ask("a", "count please", create=True)
        session_a = box.session_id("a")
        box.ask(session_a, "count again")
        box.ask("b", "count please", create=True)
        results = box.tool_results()
        report["tool_results"] = results
        report["same_session_keeps_state"] = (
            len(results) >= 2 and results[0][0] == 1 and results[1][0] == 2 and results[0][1] == results[1][1]
        )
        report["other_session_starts_fresh"] = (
            len(results) >= 3 and results[2][0] == 1 and results[2][1] != results[0][1]
        )
        report["instructions_in_system_prompt"] = any(
            "<mcp-server-instructions>" in system and "Counter server: call count" in system
            for system in box.systems()
        )
        pid_a = results[0][1] if results else None
        pid_b = results[2][1] if len(results) >= 3 else None
        box.ipc({"command": "delete_session", "target": {"kind": "id", "id": session_a}})
        a_gone = pid_a is not None and wait_for(lambda: not alive(pid_a), 15)
        report["delete_session_reaps_its_process"] = bool(a_gone and pid_b is not None and alive(pid_b))
        starts = box.marker_pids()
        report["server_starts"] = len(starts)
        box.stop_daemon()
        report["daemon_stop_leaves_no_orphans"] = bool(
            starts and wait_for(lambda: not any(alive(pid) for pid in starts), 15)
        )
    except Exception as error:  # noqa: BLE001 — 测具中途出错也要记成失败
        report["aborted"] = repr(error)
        log = box.home / "daemon.log"
        if log.exists():
            report["daemon_log_tail"] = log.read_text()[-1500:]
    finally:
        box.close()
    checks = [key for key, value in report.items() if isinstance(value, bool)]
    print(json.dumps(report, ensure_ascii=False, indent=2))
    passed = sum(1 for key in checks if report[key])
    print(f"{passed}/{len(checks)} passed" if "aborted" not in report else "aborted")
    return 0 if checks and passed == len(checks) and "aborted" not in report else 1


if __name__ == "__main__":
    sys.exit(main())

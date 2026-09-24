#!/usr/bin/env python3
"""跨会话消息黑盒(09-23):隔离家目录 + 独立端口 daemon + 桩 LLM(stub.py)。

三条会话:发件(A)、收件(B)、关着(C)。A、B 有窗口开着(测具经 IPC 替它们报在线),
C 没有。判据:

    list_shows_open_only             A 的 list 里有 B,没有 C,没有 A 自己
    list_carries_fields              B 那条带 mode / cwd / data(conversation.db)/ open
    idle_session_gets_a_turn         A 给闲着的 B 发 PING → B 起了一轮,user_content 是外壳,
                                     from=发件、session=A 的 id
    reply_comes_back                 B 的 AI 用同一件工具回 PONG → A 读到(GOT_PONG)
    running_session_reads_it_midturn B 跑着 sleep 6 时 A 发 MIDTURN → B 在这一轮里读到,
                                     没有另起一轮
    closed_session_refused           发给关着的 C → 工具报错,C 没有起任何一轮
    list_snippet_not_envelope        会话列表里 B 的摘要不是外壳原文
    presence_expires                 停报 B 的在线,过期之后 A 的 list 里就没有 B 了
    no_orphans                       daemon 停掉后没有残留进程

用法: run.py <miyu 二进制>        全过退出码 0

隔离:/tmp 下的临时家目录 + 独立 XDG_RUNTIME_DIR + 独立端口,跑完删掉,不碰线上 8300。
"""

import json
import os
import shutil
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

HERE = Path(__file__).resolve().parent


def free_port():
    """别的会话的测具也在这台机器上起 daemon,写死端口会撞(09-23 撞过 18581)。"""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


PORT = free_port()
STUB_PORT = free_port()
results = {}


def check(name, ok, detail=""):
    results[name] = bool(ok)
    print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:300]}", flush=True)


class Sandbox:
    def __init__(self, miyu: Path):
        self.miyu = miyu
        self.home = sandbox_dir.make("miyu-xs-", delete_at_exit=False)
        self.run = self.home / "run"
        self.stub_log = self.home / "stub.jsonl"
        for path in (self.run, self.home / "config", self.home / "work"):
            path.mkdir()
        config = {
            "config_version": 3,
            "oobe_done": True,
            "active_provider": "stub",
            "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
            "providers": [{
                "id": "stub", "display_name": "Stub", "enabled": True,
                "base_url": f"http://127.0.0.1:{STUB_PORT}/v1", "protocol": "openai-chat",
                "api_key": "stub-key", "models": ["stub-model"], "default_model": "stub-model",
            }],
            "memory": {"enabled": False},
            "voice": {"enabled": False},
        }
        (self.home / "config" / "config.jsonc").write_text(json.dumps(config), encoding="utf-8")
        # HERDR_*:走查进程会继承 AI 会话所在 herdr pane 的坐标,不剥的话会往用户真
        # herdr 侧栏报桩模型的状态(main f7379843 起所有测具都剥)。
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith("HERDR_")
                    and k not in ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "MIYU_HOME",
                                  "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME")}
        self.env.update(MIYU_HOME=str(self.home), XDG_RUNTIME_DIR=str(self.run), LANG="zh_CN.UTF-8")
        self.stub = None
        self.daemon = None

    # —— 进程 ——
    def start(self):
        stub_env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        stub_env.update(STUB_PORT=str(STUB_PORT), STUB_LOG=str(self.stub_log))
        self.stub = subprocess.Popen([sys.executable, str(HERE / "stub.py")], env=stub_env,
                                     stdout=subprocess.DEVNULL, stderr=(self.home / "stub.err").open("w"))
        self.daemon = subprocess.Popen([str(self.miyu), "daemon", "--port", str(PORT)], env=self.env,
                                       cwd=str(self.home / "work"), stdin=subprocess.DEVNULL,
                                       stdout=(self.home / "daemon.log").open("w"), stderr=subprocess.STDOUT)
        if not wait_for(self.socket, 40):
            raise SystemExit("daemon 没起来,日志:\n" + (self.home / "daemon.log").read_text()[-2000:])

    def socket(self):
        return next(iter(self.run.rglob("*.sock")), None)

    def stop(self):
        subprocess.run([str(self.miyu), "daemon", "stop"], env=self.env, capture_output=True, timeout=30)
        for proc in (self.daemon, self.stub):
            if proc and proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    proc.kill()
        for sig in (signal.SIGTERM, signal.SIGKILL):
            rest = self.leftovers()
            if not rest:
                break
            for pid in rest:
                try:
                    os.kill(int(pid), sig)
                except OSError:
                    pass
            time.sleep(1)
        return self.leftovers()

    def leftovers(self):
        found = []
        for proc in Path("/proc").iterdir():
            if not proc.name.isdigit() or int(proc.name) == os.getpid():
                continue
            try:
                environ = (proc / "environ").read_bytes()
                cmdline = (proc / "cmdline").read_bytes()
            except OSError:
                continue
            if f"MIYU_HOME={self.home}".encode() in environ or str(self.home).encode() in cmdline:
                found.append(proc.name)
        return found

    # —— 与 daemon 说话 ——
    def ipc(self, command):
        payload = json.dumps({"version": 3, **command}).encode()
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(10)
            sock.connect(str(self.socket()))
            sock.sendall(struct.pack(">I", len(payload)) + payload)
            header = recv_exact(sock, 4)
            (length,) = struct.unpack(">I", header)
            return json.loads(recv_exact(sock, length))

    def ask(self, session, text, create=False, timeout=120):
        args = [str(self.miyu), "ask", "--output-format", "json", "--session", session]
        if create:
            args.append("--create")
        proc = subprocess.run([*args, text], env=self.env, stdin=subprocess.DEVNULL, cwd=str(self.home / "work"),
                              capture_output=True, text=True, timeout=timeout)
        done = {}
        for line in proc.stdout.splitlines():
            if line.strip().startswith("{"):
                done = json.loads(line)
        if proc.returncode != 0:
            raise AssertionError(f"ask {session!r} failed ({proc.returncode}): "
                                 f"stderr={proc.stderr[-600:]!r} stdout={proc.stdout[-600:]!r}")
        return done.get("text") or done.get("content") or ""

    def query(self, sql, params=()):
        rows = []
        for db in sorted(self.home.rglob("conversation.db")):
            con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
            con.row_factory = sqlite3.Row
            rows.extend(dict(r) for r in con.execute(sql, params).fetchall())
            con.close()
        return rows

    def session_id(self, name):
        rows = self.query("SELECT session_id FROM sessions WHERE name = ? AND kind = 'user'", (name,))
        return rows[0]["session_id"] if rows else None

    def turns(self, session_id):
        return self.query("SELECT * FROM turns WHERE session_id = ? ORDER BY seq", (session_id,))


def recv_exact(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise ConnectionError("daemon closed the socket")
        data += chunk
    return data


def wait_for(predicate, timeout, step=0.5):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


def tool_result(reply, prefix):
    """桩把工具结果原样接在 prefix 后面回出来。"""
    text = reply.split(prefix, 1)[1].strip() if prefix in reply else ""
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw": reply}


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    box = Sandbox(Path(sys.argv[1]).resolve())
    keep_open = threading.Event()
    try:
        scenario(box, keep_open)
    except Exception as error:  # noqa: BLE001 — 测具中途出错也要记成失败、留现场
        check("aborted", False, repr(error))
    finally:
        keep_open.clear()
        rest = box.stop()
        check("no_orphans", not rest, rest)
        if all(results.values()):
            shutil.rmtree(box.home, ignore_errors=True)
        else:
            print(f"失败现场留在 {box.home}(看完手动删；放一天也会被下次跑测具时清掉)")
    failed = [name for name, ok in results.items() if not ok]
    print(f"\n{len(results) - len(failed)}/{len(results)} 通过" + (f",失败:{failed}" if failed else ""))
    return 1 if failed else 0


def scenario(box, keep_open):
    box.start()
    for name in ("发件", "收件", "关着"):
        box.ask(name, "hello", create=True)
    a, b, c = (box.session_id(name) for name in ("发件", "收件", "关着"))
    assert a and b and c, (a, b, c)
    keep_open.set()

    def report_open():
        # 终端窗口的在线 15 秒过期,测具替 A、B 的窗口每 3 秒报一次。A 也得开着:
        # B 回话时 A 那一轮早跑完了,一次性 ask 又不报在线,不报就回不过去。
        while keep_open.is_set():
            for viewer, session in (("tk-a", a), ("tk-b", b)):
                try:
                    box.ipc({"command": "presence", "viewer": viewer, "session": session})
                except OSError:
                    pass
            time.sleep(3)

    threading.Thread(target=report_open, daemon=True).start()
    time.sleep(0.5)

    listed = tool_result(box.ask("发件", "TK list"), "LIST_RESULT")
    ids = [row.get("session_id") for row in listed.get("other_sessions", [])]
    # 这条会话单独写明(09-24 真机:名单里只剩一条时模型把它当成了自己)。
    check("list_shows_open_only",
          b in ids and c not in ids and a not in ids
          and (listed.get("this_session") or {}).get("session_id") == a,
          {"other_sessions": ids, "this_session": listed.get("this_session")})
    row_b = next((row for row in listed.get("other_sessions", []) if row.get("session_id") == b), {})
    check("list_carries_fields",
          row_b.get("mode") == "normal" and row_b.get("open") is True and row_b.get("cwd")
          and row_b.get("short_id") == b.rsplit("_", 1)[-1]
          and str(row_b.get("data", "")).endswith("conversation.db"), row_b)

    before_b = len(box.turns(b))
    # A 这一轮的收尾不一定是「SENT: …」:B 回得快的话,PONG 会插进 A 还没跑完的这一轮,
    # A 当场读到(GOT_PONG)——那也是对的。所以只看 B 那边真起了一轮。
    sent = box.ask("发件", f"TK send {b} PING from the sender")
    woke = wait_for(lambda: [t for t in box.turns(b)[before_b:] if t["user_content"].startswith("<cross-session-message")], 30)
    first = woke[0] if woke else {}
    check("idle_session_gets_a_turn",
          f'from="发件" session="{a}"' in first.get("user_content", "")
          and "PING from the sender" in first.get("user_content", "")
          and ("started a new turn there" in sent or "GOT_PONG" in sent),
          {"sender_reply": sent[:120], "user_content": first.get("user_content", "")[:200]})
    got_pong = wait_for(lambda: [t for t in box.turns(a) if "GOT_PONG" in (t["assistant_content"] or "")], 40)
    check("reply_comes_back", bool(got_pong),
          [t["assistant_content"][:120] for t in box.turns(a)[-3:]])

    # B 跑着一轮(sleep 6)时 A 发话:应当插进 B 这一轮,不另起。
    slow = threading.Thread(target=lambda: box.ask("收件", "TK slow", timeout=90), daemon=True)
    before_b = len(box.turns(b))
    slow.start()
    wait_for(lambda: any(t["status"] == "running" for t in box.turns(b)[before_b:]), 20, step=0.2)
    time.sleep(1)
    box.ask("发件", f"TK send {b} MIDTURN note while you work")
    slow.join(timeout=90)
    new_b = box.turns(b)[before_b:]
    replies = [t["assistant_content"] for t in new_b]
    check("running_session_reads_it_midturn",
          len(new_b) == 1 and "SAW_MIDTURN" in (new_b[0]["assistant_content"] or ""),
          replies)

    before_c = len(box.turns(c))
    refused = box.ask("发件", f"TK send {c} anyone there")
    time.sleep(1)
    check("closed_session_refused",
          "not an open or running session" in refused and len(box.turns(c)) == before_c,
          refused[:200])

    listing = box.ipc({"command": "list_sessions", "mode": "all"})
    rows = (listing.get("data") or {}).get("sessions") or []
    snippet_b = next((row.get("last_user_content") for row in rows if row.get("session_id") == b), None)
    check("list_snippet_not_envelope",
          snippet_b is not None and not str(snippet_b).startswith("<cross-session-message"), snippet_b)

    keep_open.clear()
    time.sleep(17)
    listed = tool_result(box.ask("发件", "TK list"), "LIST_RESULT")
    ids = [row.get("session_id") for row in listed.get("other_sessions", [])]
    check("presence_expires", b not in ids, ids)


if __name__ == "__main__":
    sys.exit(main())

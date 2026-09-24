#!/usr/bin/env python3
"""真模型走查:被打断的轮、轮中插话之后,下一轮还吃不吃得到前缀缓存(09-24)。

隔离家目录 + 独立端口 daemon,供应商用本机配置里的 `deepseek`(官方 deepseek-flash,
key 从 ~/.miyu/config/config.jsonc 读、只写进临时家目录、跑完删掉)。三个会话:

    baseline    正常的工具轮 → 下一轮
    interrupted 第二个工具还在跑时按停止 → 下一轮
    followup    第一个工具跑着时又发一句(并进这一轮) → 这一轮跑完 → 下一轮

每个会话看最后一轮第一条请求(cache-usage.jsonl):
    pure_append  前缀指纹 same >= prev(上一条请求的每条消息原样还在)
    cache_hit    cache_read 盖住了上一条请求的 prompt(差不过 256 token:末尾不满块的零头)

用法: replay_live.py <miyu 二进制> [标签]   同一脚本对新旧两个二进制各跑一次对比;
      逐请求记账行存到 ~/.cache/miyu-cache-replay/live-<标签>.json
"""

import json
import os
import re
import shutil
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CONFIG = Path.home() / ".miyu/config/config.jsonc"


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def wait_for(predicate, timeout, step=0.5):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


def recv_exact(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise OSError("socket closed")
        data += chunk
    return data


def deepseek_provider():
    text = CONFIG.read_text(encoding="utf-8")
    text = re.sub(r"(?m)^\s*//.*$", "", text)
    text = re.sub(r",(\s*[}\]])", r"\1", text)
    for provider in json.loads(text).get("providers", []):
        if provider.get("id") == "deepseek":
            return provider
    sys.exit("本机配置里没有 id=deepseek 的供应商")


class Sandbox:
    def __init__(self, miyu):
        self.miyu = miyu
        self.port = free_port()
        self.home = Path(tempfile.mkdtemp(prefix="miyu-replay-live-", dir=str(Path.home() / ".cache")))
        self.run = self.home / "run"
        self.work = self.home / "work"
        for path in (self.run, self.home / "config", self.work):
            path.mkdir()
        source = deepseek_provider()
        provider = {
            "id": "deepseek", "display_name": "DeepSeek", "enabled": True,
            "base_url": source["base_url"], "protocol": source.get("protocol") or "openai-chat",
            "api_key": source["api_key"], "models": ["deepseek-flash"], "default_model": "deepseek-flash",
        }
        config = {
            "config_version": 3,
            "oobe_done": True,
            "active_provider": "deepseek",
            "active_provider_models": [{"provider_id": "deepseek", "model": "deepseek-flash"}],
            "providers": [provider],
            "memory": {"enabled": False},
            "voice": {"enabled": False},
        }
        (self.home / "config" / "config.jsonc").write_text(json.dumps(config), encoding="utf-8")
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith("HERDR_")
                    and k not in ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "MIYU_HOME",
                                  "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME")}
        self.env.update(MIYU_HOME=str(self.home), XDG_RUNTIME_DIR=str(self.run), LANG="zh_CN.UTF-8",
                        MIYU_LOG="info")
        self.daemon = None
        self.clients = []

    def start(self):
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
            sock.settimeout(10)
            sock.connect(str(sock_path))
            sock.sendall(struct.pack(">I", len(payload)) + payload)
            (length,) = struct.unpack(">I", recv_exact(sock, 4))
            return json.loads(recv_exact(sock, length))

    def ask(self, session, text, create=False, timeout=240, mode=None):
        args = [str(self.miyu), "ask", "--output-format", "json", "--session", session]
        if create:
            args.append("--create")
        if mode:
            args += ["--mode", mode]
        proc = subprocess.run([*args, text], env=self.env, stdin=subprocess.DEVNULL, cwd=str(self.work),
                              capture_output=True, text=True, timeout=timeout)
        if proc.returncode != 0:
            raise AssertionError(f"ask {session!r} failed ({proc.returncode}): {proc.stderr[-600:]!r}")

    def ask_in_background(self, session, text):
        client = subprocess.Popen([str(self.miyu), "ask", "--output-format", "json", "--session", session, text],
                                  env=self.env, stdin=subprocess.DEVNULL, cwd=str(self.work),
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.clients.append(client)
        return client

    def stop_running(self, session_id):
        overview = self.ipc({"command": "jobs_overview"})
        run_ids = [r.get("run_id") for r in (overview.get("data") or {}).get("peer_runs", [])
                   if r.get("session_id") == session_id]
        if not run_ids:
            run_ids = re.findall(r'"run_id":\s*"(run_[^"]+)"', json.dumps(overview))
        for run_id in run_ids:
            self.ipc({"command": "cancel", "run_id": run_id})
        return run_ids

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
        return self.query("SELECT turn_id, status, user_content FROM turns WHERE session_id = ? ORDER BY seq",
                          (session_id,))

    def counter(self, marker):
        path = self.work / "counter.txt"
        if not path.exists():
            return 0
        return sum(1 for line in path.read_text().splitlines() if line.strip() == marker)

    def cache_rows(self, session_id):
        rows = []
        for path in sorted(self.home.rglob("cache-usage.*.jsonl")):
            for line in path.read_text(encoding="utf-8").splitlines():
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if row.get("sess") == session_id and row.get("scope") == "chat":
                    rows.append(row)
        return rows

    def stop(self):
        for proc in (*self.clients, self.daemon):
            if proc and proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    proc.kill()


OUTPUT_LINES = 1200


def tool_prompt(marker, last_sleep):
    """三次工具调用,每次输出约 1200 行(三千来 token):被打断/插话的那一轮本身要有分量,
    修前修后下一轮首请求的命中率才拉得开。第三次调用睡 `last_sleep` 秒,留出打断的窗口。"""
    calls = []
    for index in (1, 2, 3):
        sleep = last_sleep if index == 3 else 2
        calls.append(f"call {index}: `echo {marker}{index} >> counter.txt; seq 1 {OUTPUT_LINES}; sleep {sleep}`")
    return (
        "Use the run_command tool three times, one command per call, waiting for each result "
        "before the next call. " + " ".join(calls) + ". Then reply with one short sentence."
    )


def verdict(box, name, session_id):
    """最后一轮第一条请求 vs 它之前那条请求(日志里上一行)。"""
    rows = box.cache_rows(session_id)
    if len(rows) < 2:
        return {"name": name, "error": f"only {len(rows)} logged requests"}
    last = rows[-1]
    # 最后一轮是一句话的回答:没有工具,只有一条请求。
    before = rows[-2]
    prev = last.get("prev")
    same = last.get("same")
    pure_append = prev is not None and same is not None and same >= prev
    expected = before.get("prompt") or 0
    covered = expected - (last.get("cache_read") or 0) <= 256
    return {
        "name": name,
        "pure_append": pure_append,
        "cache_hit": covered,
        "prev": prev,
        "same": same,
        "at": last.get("at"),
        "role": last.get("role"),
        "prompt": last.get("prompt"),
        "cache_read": last.get("cache_read"),
        "previous_prompt": expected,
    }


def scenario(box):
    box.start()
    for name in ("baseline", "interrupted", "followup"):
        box.ask(name, "Reply with the single word: ready.", create=True)
    baseline, interrupted, followup = (box.session_id(n) for n in ("baseline", "interrupted", "followup"))

    # 正常的工具轮 → 下一轮
    box.ask(baseline, tool_prompt("B", 1))
    box.ask(baseline, "Reply with the single word: thanks.")

    # 第二个工具还在跑时按停止 → 下一轮
    box.ask_in_background(interrupted, tool_prompt("I", 60))
    if not wait_for(lambda: box.counter("I3") > 0, 240):
        raise AssertionError("the third command of the interrupted turn never started")
    time.sleep(1)
    box.stop_running(interrupted)
    wait_for(lambda: all(t["status"] != "running" for t in box.turns(interrupted)), 30)
    box.ask(interrupted, "Reply with the single word: thanks.")

    # 第一个工具跑着时又发一句 → 这一轮跑完 → 下一轮
    box.ask_in_background(followup, tool_prompt("F", 1))
    if not wait_for(lambda: box.counter("F1") > 0, 180):
        raise AssertionError("the first command of the followup turn never started")
    box.ask_in_background(followup, "Also mention the word banana in your final sentence.")
    wait_for(lambda: box.counter("F3") > 0, 240)
    wait_for(lambda: len(box.turns(followup)) >= 2
             and all(t["status"] != "running" for t in box.turns(followup)), 180)
    time.sleep(2)
    box.ask(followup, "Reply with the single word: thanks.")

    results = [verdict(box, name, sid) for name, sid in
               (("baseline", baseline), ("interrupted", interrupted), ("followup", followup))]
    # 插话那个会话:第二句必须是在第一轮进行中并进去的,不然这个场景没测到。
    merged = box.query("SELECT count(*) AS n FROM queued_prompts WHERE session_id = ? AND status = 'consumed'",
                       (followup,))
    results[2]["followup_merged"] = bool(merged and merged[0]["n"] > 0)
    cut = [t for t in box.turns(interrupted) if t["status"] == "interrupted"]
    results[1]["turn_interrupted"] = bool(cut)
    for result, sid in zip(results, (baseline, interrupted, followup)):
        result["requests"] = box.cache_rows(sid)
    return results


def print_table(result):
    """逐请求命中表;最后一行就是被测的那条「下一轮首请求」。"""
    print(f"\n[{result['name']}]")
    print(f"  {'#':>2} {'msgs':>5} {'prev→same':>10} {'prompt':>7} {'cache_read':>10} {'hit%':>6}")
    rows = result.get("requests", [])
    for index, row in enumerate(rows, 1):
        prompt = row.get("prompt") or 0
        read = row.get("cache_read") or 0
        prev_same = f"{row.get('prev', '-')}→{row.get('same', '-')}"
        mark = "  ← 下一轮首请求" if index == len(rows) else ""
        rate = f"{100 * read / prompt:.1f}" if prompt else "-"
        print(f"  {index:>2} {row.get('msgs', '-'):>5} {prev_same:>10} {prompt:>7} {read:>10} {rate:>6}{mark}")


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__)
        return 2
    box = Sandbox(Path(sys.argv[1]).resolve())
    label = sys.argv[2] if len(sys.argv) > 2 else "run"
    ok = False
    try:
        results = scenario(box)
        # 家目录里有 key,整个删;逐请求的记账行(不含正文与 key)先抄出来留证。
        out = Path.home() / ".cache/miyu-cache-replay" / f"live-{label}.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(results, ensure_ascii=False, indent=1), encoding="utf-8")
        for result in results:
            print_table(result)
            summary = {k: v for k, v in result.items() if k != "requests"}
            print("  " + json.dumps(summary, ensure_ascii=False))
        ok = all(r.get("pure_append") and r.get("cache_hit") for r in results) \
            and results[1].get("turn_interrupted") and results[2].get("followup_merged")
    except Exception as error:  # noqa: BLE001 — 测具中途出错也要记成失败
        print(f"❌ aborted: {error!r}")
    finally:
        box.stop()
        # 家目录里有 API key:不论成败都删。
        shutil.rmtree(box.home, ignore_errors=True)
    print("✅ all pure append" if ok else "❌ some turn did not replay as a pure append")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

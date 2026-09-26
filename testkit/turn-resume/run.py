#!/usr/bin/env python3
"""断点续跑黑盒(09-24):隔离家目录 + 独立端口 daemon + 桩 LLM(stub.py)。

回合跑到一半(桩让它跑一条 sleep 30 的命令)时把 daemon 干掉,再起一个新的:

    crash_marks_the_orphan        SIGKILL → 那一轮收成「已中断」,流水账里有认领,结案 resumed
    crash_resumes_the_turn        新 daemon 替会话起了续跑轮(<service-restart attempt="1">),跑完了
    model_sees_the_cut_off_tool   续跑那次请求里,没跑完的那次工具调用标着被打断,还有中断回放那段说明
    tool_not_replayed             那条命令只跑过一次(counter.txt 里只有一行),系统没替模型重放
    graceful_keeps_the_claim      SIGTERM(有序关停)同样接着跑;被打断那一轮的用量记下了
    user_stop_is_final            人按停止的那一轮,重启后不续、也不留认领
    subagent_resumes              子代理:子会话里那一轮被打断,新 daemon 把它挂回父会话名下接着跑,命令没重放
    parent_turn_not_held          派它的那一轮派完就收了(09-26 起子代理只在后台跑),daemon 被杀时没有要续的
    parent_gets_the_subagent_result 子代理跑完,结果照后台任务那套送回父会话
    goal_round_resumes            /goal 续轮被打断:这一轮带着续轮的身份接着跑
    goal_keeps_going              续完那一轮,自动续轮也恢复了:接着开了下一轮
    crashloop_keeps_counting      续跑的那一轮每次都被打断:次数一直往上数,第 4 次照样接着跑(不设上限)
    no_orphans                    收尾后没有残留进程

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
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


PORT = free_port()
STUB_PORT = free_port()
results = {}


def check(name, ok, detail=""):
    results[name] = bool(ok)
    print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:300]}", flush=True)


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
            raise ConnectionError("daemon closed the socket")
        data += chunk
    return data


class Sandbox:
    def __init__(self, miyu: Path):
        self.miyu = miyu
        self.home = Path(tempfile.mkdtemp(prefix="miyu-resume-", dir="/tmp"))
        self.run = self.home / "run"
        self.work = self.home / "work"
        self.stub_log = self.home / "stub.jsonl"
        for path in (self.run, self.home / "config", self.work):
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
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith("HERDR_")
                    and k not in ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "MIYU_HOME",
                                  "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME")}
        self.env.update(MIYU_HOME=str(self.home), XDG_RUNTIME_DIR=str(self.run), LANG="zh_CN.UTF-8",
                        MIYU_LOG="info")
        self.stub = None
        self.daemon = None
        self.clients = []
        self.boots = 0

    # —— 进程 ——
    def start_stub(self):
        stub_env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        stub_env.update(STUB_PORT=str(STUB_PORT), STUB_LOG=str(self.stub_log))
        self.stub = subprocess.Popen([sys.executable, str(HERE / "stub.py")], env=stub_env,
                                     stdout=subprocess.DEVNULL, stderr=(self.home / "stub.err").open("w"))

    def start_daemon(self):
        """直接起 `__daemon`:pid 就是 daemon 本身,SIGKILL / SIGTERM 打得准。"""
        self.boots += 1
        log = (self.home / f"daemon-{self.boots}.log").open("w")
        self.daemon = subprocess.Popen([str(self.miyu), "__daemon", "--port", str(PORT)], env=self.env,
                                       cwd=str(self.work), stdin=subprocess.DEVNULL, stdout=log,
                                       stderr=subprocess.STDOUT)
        if not wait_for(self.ping, 40):
            raise SystemExit(f"daemon 没起来,日志:\n{(self.home / f'daemon-{self.boots}.log').read_text()[-2000:]}")

    def kill_daemon(self, sig):
        self.daemon.send_signal(sig)
        try:
            self.daemon.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.daemon.kill()
            self.daemon.wait(timeout=10)
        for client in self.clients:
            if client.poll() is None:
                client.kill()
        self.clients.clear()

    def ping(self):
        try:
            return self.ipc({"command": "ping"}) is not None
        except OSError:
            return False

    def stop(self):
        for proc in (*self.clients, self.daemon, self.stub):
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

    def ask(self, session, text, create=False, timeout=120):
        args = [str(self.miyu), "ask", "--output-format", "json", "--session", session]
        if create:
            args.append("--create")
        proc = subprocess.run([*args, text], env=self.env, stdin=subprocess.DEVNULL, cwd=str(self.work),
                              capture_output=True, text=True, timeout=timeout)
        if proc.returncode != 0:
            raise AssertionError(f"ask {session!r} failed ({proc.returncode}): {proc.stderr[-600:]!r}")

    def ask_in_background(self, session, text):
        """跑慢工具的那一轮:客户端挂在后台,daemon 被杀时它跟着断。"""
        client = subprocess.Popen([str(self.miyu), "ask", "--output-format", "json", "--session", session, text],
                                  env=self.env, stdin=subprocess.DEVNULL, cwd=str(self.work),
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.clients.append(client)

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

    def events(self, turn_id, kind):
        return self.query("SELECT text_payload FROM turn_journal_events WHERE turn_id = ? AND kind = ? "
                          "ORDER BY event_id", (turn_id, kind))

    def counter(self, marker):
        path = self.work / "counter.txt"
        if not path.exists():
            return 0
        return sum(1 for line in path.read_text().splitlines() if line.strip() == marker)

    def stub_requests(self):
        if not self.stub_log.exists():
            return []
        return [json.loads(line) for line in self.stub_log.read_text().splitlines() if line.strip()]


def resume_outcome(box, turn_id):
    rows = box.events(turn_id, "restart_resume")
    return json.loads(rows[-1]["text_payload"]) if rows else None


def restart_turns(box, session):
    return [t for t in box.turns(session) if t["user_content"].startswith("<service-restart")]


def interrupt_mid_tool(box, session, text, marker, sig):
    """在 session 里起一轮慢工具,等命令真跑起来(counter 里有了记号)再发信号,然后起新 daemon。"""
    before = box.counter(marker)
    box.ask_in_background(session, text)
    started = wait_for(lambda: box.counter(marker) > before, 60)
    if not started:
        raise AssertionError(f"slow tool {marker} never started")
    time.sleep(1)
    box.kill_daemon(sig)
    box.start_daemon()


def scenario(box):
    box.start_stub()
    box.start_daemon()
    names = ("崩溃", "关停", "停止", "子代理", "目标", "循环")
    for name in names:
        box.ask(name, "hello", create=True)
    crash, graceful, stopped, parent, goal, loop = (box.session_id(n) for n in names)
    assert all((crash, graceful, stopped, parent, goal, loop)), (crash, graceful, stopped, parent, goal, loop)

    # —— SIGKILL:进程说没就没 ——
    interrupt_mid_tool(box, crash, "TK slow C1", "C1", signal.SIGKILL)
    resumed = wait_for(lambda: [t for t in restart_turns(box, crash) if t["status"] == "completed"], 60)
    cut = [t for t in box.turns(crash) if "TK slow C1" in t["user_content"]]
    cut = cut[0] if cut else {}
    outcome = resume_outcome(box, cut.get("turn_id", ""))
    check("crash_marks_the_orphan",
          cut.get("status") == "interrupted" and box.events(cut.get("turn_id", ""), "restart_orphaned")
          and outcome and outcome.get("outcome") == "resumed",
          {"status": cut.get("status"), "outcome": outcome})
    check("crash_resumes_the_turn",
          resumed and resumed[0]["user_content"].startswith('<service-restart attempt="1">')
          and "RESUMED attempt=1" in resumed[0]["assistant_content"],
          [(t["user_content"][:40], t["status"], t["assistant_content"][:60]) for t in restart_turns(box, crash)])
    continuation = [r for r in box.stub_requests() if r["user_head"].startswith('<service-restart attempt="1"')]
    check("model_sees_the_cut_off_tool",
          continuation and continuation[0]["interrupted_tool"] and continuation[0]["recovery_note"],
          continuation[:1])
    time.sleep(2)
    check("tool_not_replayed", box.counter("C1") == 1, f"C1 ran {box.counter('C1')} times")

    # —— SIGTERM:有序关停,认领留着,用量记下 ——
    interrupt_mid_tool(box, graceful, "TK slow G1", "G1", signal.SIGTERM)
    resumed = wait_for(lambda: [t for t in restart_turns(box, graceful) if t["status"] == "completed"], 60)
    cut = [t for t in box.turns(graceful) if "TK slow G1" in t["user_content"]]
    cut = cut[0] if cut else {}
    outcome = resume_outcome(box, cut.get("turn_id", ""))
    check("graceful_keeps_the_claim",
          resumed and cut.get("status") == "interrupted" and outcome and outcome.get("outcome") == "resumed"
          and (cut.get("token_total") or 0) > 0 and box.counter("G1") == 1,
          {"status": cut.get("status"), "outcome": outcome, "token_total": cut.get("token_total"),
           "ran": box.counter("G1")})

    # —— 人按停止:停了就是停了 ——
    box.ask_in_background(stopped, "TK slow S1")
    wait_for(lambda: box.counter("S1") > 0, 60)
    overview = box.ipc({"command": "jobs_overview"})
    runs = json.dumps(overview)
    run_ids = [r.get("run_id") for r in (overview.get("data") or {}).get("peer_runs", [])
               if r.get("session_id") == stopped]
    if not run_ids:
        # 单次 ask 起的轮在不同版本里挂的地方不一样,退一步从整包里捞
        import re
        run_ids = re.findall(r'"run_id":\s*"(run_[^"]+)"', runs)
    for run_id in run_ids:
        box.ipc({"command": "cancel", "run_id": run_id})
    wait_for(lambda: all(t["status"] != "running" for t in box.turns(stopped)), 30)
    box.kill_daemon(signal.SIGTERM)
    box.start_daemon()
    time.sleep(8)
    cut = [t for t in box.turns(stopped) if "TK slow S1" in t["user_content"]]
    cut = cut[0] if cut else {}
    check("user_stop_is_final",
          cut.get("status") == "interrupted" and not restart_turns(box, stopped)
          and not box.events(cut.get("turn_id", ""), "restart_orphaned"),
          {"status": cut.get("status"), "run_ids": run_ids, "restart_turns": len(restart_turns(box, stopped))})

    # —— 子代理:子会话里的命令睡着时被打断。09-26 起只在后台跑,派它的那一轮早就收了 ——
    interrupt_mid_tool(box, parent, "TK sub SUB1", "SUB1", signal.SIGKILL)
    children = box.query("SELECT session_id FROM sessions WHERE kind = 'subagent' AND parent_session_id = ?",
                         (parent,))
    child = children[0]["session_id"] if children else ""
    child_resumed = wait_for(lambda: [t for t in restart_turns(box, child) if t["status"] == "completed"], 90)
    check("subagent_resumes",
          child_resumed and "RESUMED attempt=1" in (child_resumed[0]["assistant_content"] or "")
          and box.counter("SUB1") == 1,
          {"child": child, "restarts": [(t["user_content"][:40], t["status"]) for t in restart_turns(box, child)],
           "ran": box.counter("SUB1")})
    dispatch = [t for t in box.turns(parent) if "TK sub SUB1" in t["user_content"]]
    check("parent_turn_not_held",
          bool(dispatch) and dispatch[0]["status"] == "completed" and not restart_turns(box, parent),
          {"dispatch": [(t["status"], (t["assistant_content"] or "")[:40]) for t in dispatch],
           "parent_restarts": [(t["user_content"][:60], t["status"]) for t in restart_turns(box, parent)]})
    got = wait_for(lambda: [t for t in box.turns(parent) if "GOT REPORT" in (t["assistant_content"] or "")], 90)
    check("parent_gets_the_subagent_result", got,
          [(t["user_content"][:40], t["status"], (t["assistant_content"] or "")[:60]) for t in box.turns(parent)])

    # —— /goal 续轮:这一轮接着跑,自动续轮也恢复 ——
    target = {"kind": "id", "id": goal}
    box.ipc({"command": "goal", "target": target, "input": "TK goal work"})
    if not wait_for(lambda: box.counter("GR") > 0, 60):
        raise AssertionError("the first goal round never started")
    time.sleep(1)
    box.kill_daemon(signal.SIGKILL)
    box.start_daemon()
    resumed = wait_for(lambda: [t for t in box.turns(goal)
                                if t["user_content"].startswith('<service-restart attempt="1" goal-round="true">')
                                and t["status"] == "completed"], 60)
    check("goal_round_resumes", resumed and box.counter("GR-resumed") == 1 and box.counter("GR") == 1,
          {"turns": [(t["user_content"][:48], t["status"]) for t in box.turns(goal)],
           "GR": box.counter("GR"), "resumed": box.counter("GR-resumed")})
    after = resumed[0]["seq"] if resumed else 1 << 30
    later = wait_for(lambda: [t for t in box.turns(goal)
                              if t["user_content"].startswith("<goal_round>") and t["seq"] > after], 60)
    check("goal_keeps_going", later, [(t["seq"], t["user_content"][:40]) for t in box.turns(goal)])
    box.ipc({"command": "goal", "target": target, "input": "pause"})

    # —— 续跑的那一轮每次都被打断:次数一直往上数,不设上限 ——
    interrupt_mid_tool(box, loop, "TK crashloop", "L", signal.SIGKILL)
    for attempt in (1, 2, 3):
        started = wait_for(lambda: len(restart_turns(box, loop)) >= attempt and box.counter("L") >= attempt + 1, 60)
        if not started:
            break
        time.sleep(1)
        box.kill_daemon(signal.SIGKILL)
        box.start_daemon()
    fourth = wait_for(lambda: [t for t in restart_turns(box, loop)
                               if t["user_content"].startswith('<service-restart attempt="4">')], 60)
    loop_restarts = restart_turns(box, loop)
    third = next((t for t in loop_restarts if t["user_content"].startswith('<service-restart attempt="3">')), {})
    outcome = resume_outcome(box, third.get("turn_id", ""))
    check("crashloop_keeps_counting",
          fourth and outcome and outcome.get("outcome") == "resumed",
          {"restarts": [t["user_content"][:32] for t in loop_restarts], "third_outcome": outcome,
           "L_runs": box.counter("L")})


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    box = Sandbox(Path(sys.argv[1]).resolve())
    try:
        scenario(box)
    except Exception as error:  # noqa: BLE001 — 测具中途出错也要记成失败、留现场
        check("aborted", False, repr(error))
    finally:
        rest = box.stop()
        check("no_orphans", not rest, rest)
        if all(results.values()):
            shutil.rmtree(box.home, ignore_errors=True)
        else:
            print(f"失败现场留在 {box.home}(看完手动删)")
    failed = [name for name, ok in results.items() if not ok]
    print(f"\n{len(results) - len(failed)}/{len(results)} 通过" + (f",失败:{failed}" if failed else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

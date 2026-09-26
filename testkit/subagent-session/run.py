#!/usr/bin/env python3
"""子代理会话化黑盒(09-18):隔离 home + 独立端口 daemon + 桩 LLM(stub.py)。

子代理不再是工具层的小循环,而是挂在父会话下的真会话;孙代理再挂一层;「任务完成」=
子会话没有活动回合且名下没有未完成的后台任务。判据:

    child_result_reaches_parent            子代理(09-26 起只在后台跑)的结论经汇报叫醒回到父会话;一次性 JSON
                                           输出等整棵子树跑完,终态就是那一轮
    child_session_rows                     子会话 kind=subagent、depth=1、parent=主会话、done
    parent_turn_links_child_session        父回合 tool_flow 里落了 child_session_id
    grandchild_rows                        孙代理 depth=2、挂在子代理下、done
    grandchild_has_no_subagent_tool        孙代理的工具面里没有 subagent(桩记录的工具表)
    child_waits_for_background_command     子代理起了后台命令再结束回合 → waiting → 命令完成
                                           唤醒子代理 → done → 父才拿到最终结论
    grandchild_background_wakes_child_not_parent  后台孙代理完成唤醒的是子代理,不是主会话
    background_child_wakes_parent_with_report     后台子代理完成 → 主会话被合成轮唤醒,报告带结论
    ctrl_c_while_waiting_stops_the_subtree 一次性命令等子代理时按 Ctrl+C → 子会话那一轮取消、标中断,命令按取消退出
    restart_marks_interrupted_and_reply_resumes   daemon 重启把 waiting 的子代理标 interrupted;
                                           回复它就接着跑并进 done
    tokens_rollup_recursive                主会话累计 = 三层 turns 之和
    delete_cascades_tree                   删主会话连整棵子代理树一起删
    no_orphans                             daemon 停掉后没有测试命令残留

    cargo build
    python3 testkit/subagent-session/run.py

绝不触碰线上 8300 daemon;产物在 ~/.cache/miyu-subagent-session/。
"""
import importlib.util
import json
import os
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu")
BASE = Path(__file__).resolve().parent
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-subagent-session")).expanduser()
HOME = OUT / "home"
# unix socket 有 SUN_LEN(108B)上限,运行目录放短路径。
RUN = Path.home() / ".cache" / "miyu-sas-run"
PORT = int(os.environ.get("PORT", "18548"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18547"))
STUB_LOG = OUT / "stub.jsonl"

spec = importlib.util.spec_from_file_location("persona_ab", REPO / "testkit" / "persona-ab" / "run.py")
persona_ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(persona_ab)

results = {}
ASK_SECONDS = []


def check(name, ok, detail=""):
    results[name] = bool(ok)
    print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:260]}", flush=True)


def build_home():
    # 只清自己的家目录、运行目录和桩日志：`OUT` 在走查账里是各项共用的产物目录，整个删掉会把
    # 排在前面那些走查的截屏一起抹了（09-26）。
    for path in (HOME, RUN):
        if path.exists():
            shutil.rmtree(path)
    STUB_LOG.unlink(missing_ok=True)
    (HOME / "config").mkdir(parents=True)
    RUN.mkdir(parents=True)
    OUT.mkdir(parents=True, exist_ok=True)
    cfg = persona_ab.load_real_config()
    for key in ("platforms", "web", "voice", "alarm", "terminal_session_mode"):
        cfg.pop(key, None)
    cfg["providers"] = [{
        "enabled": True, "id": "stub", "display_name": "Stub",
        "base_url": f"http://127.0.0.1:{STUB_PORT}/v1", "protocol": "openai-chat",
        "api_key": "stub-key", "models": ["stub-model"],
    }]
    cfg["active_provider"] = "stub"
    cfg["active_provider_models"] = [{"provider_id": "stub", "model": "stub-model"}]
    cfg.pop("active_multimodal_provider_models", None)
    cfg.pop("model_tiers", None)
    cfg.setdefault("prompt", {})["active_persona"] = ""
    cfg.setdefault("memory", {})["enabled"] = False
    cfg.setdefault("cache", {})["request_log"] = False
    cfg.setdefault("display", {})["show_token_usage"] = False
    (HOME / "config" / "config.jsonc").write_text(json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8")
    real_cache = Path.home() / ".miyu" / "cache" / "models_cache.json"
    if real_cache.exists():
        (HOME / "cache").mkdir(parents=True, exist_ok=True)
        shutil.copy(real_cache, HOME / "cache" / "models_cache.json")


def env():
    e = dict(os.environ)
    e["MIYU_HOME"] = str(HOME)
    e["XDG_RUNTIME_DIR"] = str(RUN)
    for key in ("MIYU_DIRECT", "MIYU_SESSION", "MIYU_TURN_MODE", "XDG_CACHE_HOME", "XDG_CONFIG_HOME",
                "XDG_DATA_HOME", "XDG_STATE_HOME"):
        e.pop(key, None)
    e["LANG"] = "zh_CN.UTF-8"
    e["MIYU_LOG"] = os.environ.get("MIYU_LOG", "info")
    return e


def find_socket():
    for p in RUN.rglob("*.sock"):
        return p
    return None


def cli(args, timeout=180):
    # stdin 显式给 /dev/null:CLI 会探测管道 stdin 最多等 5 秒。
    proc = subprocess.run([str(BIN), *args], env=env(), stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=timeout)
    return proc.returncode, proc.stdout, proc.stderr


def ask(session, text, create=False, timeout=180, mode=None):
    args = ["ask", "--output-format", "json", "--session", session]
    if create:
        args.append("--create")
    if mode:
        args += ["--mode", mode]
    started = time.time()
    code, out, err = cli([*args, text], timeout=timeout)
    ASK_SECONDS.append(round(time.time() - started, 2))
    if code != 0:
        raise AssertionError(f"ask failed ({code}): {err[-500:]}")
    done = None
    for line in out.splitlines():
        line = line.strip()
        if line.startswith("{"):
            done = json.loads(line)
    return done or {}


def spawn_ask(session, text, create=False, mode=None):
    """在后台起一条一次性命令(它会等整棵子树跑完才退出)。"""
    args = ["ask", "--output-format", "json", "--session", session]
    if create:
        args.append("--create")
    if mode:
        args += ["--mode", mode]
    return subprocess.Popen([str(BIN), *args, text], env=env(), stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def interrupt(proc):
    """等子代理的一次性命令按 Ctrl+C 收场:整棵子树跟着停。"""
    proc.send_signal(signal.SIGINT)
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def reply_text(done):
    return done.get("text") or done.get("content") or ""


def dbs():
    return sorted(HOME.rglob("conversation.db"))


def query(sql, params=()):
    rows = []
    for db in dbs():
        try:
            con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
            con.row_factory = sqlite3.Row
            rows.extend(dict(r) for r in con.execute(sql, params).fetchall())
            con.close()
        except sqlite3.Error as error:
            print("  db error", db, error)
    return rows


def session_by_name(name):
    rows = query("SELECT * FROM sessions WHERE name = ? AND kind = 'user'", (name,))
    return rows[0] if rows else None


def children_of(session_id):
    return query("SELECT * FROM sessions WHERE parent_session_id = ? ORDER BY created_at", (session_id,))


def child_by_directive(parent_id, directive):
    """父会话下第一条 user 消息以 directive 开头的子会话。"""
    for row in children_of(parent_id):
        turns = query("SELECT user_content FROM turns WHERE session_id = ? ORDER BY seq LIMIT 1", (row["session_id"],))
        if turns and turns[0]["user_content"].startswith(directive):
            return row
    return None


def turns_of(session_id):
    return query("SELECT * FROM turns WHERE session_id = ? AND is_summary = 0 ORDER BY seq", (session_id,))


def wait_for(predicate, timeout, step=0.5):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


def stub_records():
    if not STUB_LOG.exists():
        return []
    return [json.loads(l) for l in STUB_LOG.read_text(encoding="utf-8").splitlines() if l.strip()]


def start_daemon():
    daemon = subprocess.Popen([str(BIN), "daemon", "--port", str(PORT)], env=env(), cwd=str(HOME),
                              stdin=subprocess.DEVNULL,
                              stdout=(OUT / f"daemon-{int(time.time())}.log").open("w"), stderr=subprocess.STDOUT)
    assert wait_for(find_socket, 40), "daemon socket never appeared"
    time.sleep(1.0)
    return daemon


def stop_daemon(daemon):
    subprocess.run([str(BIN), "daemon", "stop"], env=env(), capture_output=True, timeout=30)
    try:
        daemon.wait(timeout=20)
    except subprocess.TimeoutExpired:
        daemon.kill()
    for p in RUN.rglob("*.sock"):
        p.unlink(missing_ok=True)


def ipc_request(command):
    """直接对 daemon 的 unix socket 发一条命令:4 字节大端长度 + JSON(见 miyu_core::ipc::send),
    客户端先说话(没有握手帧),daemon 回一帧。"""
    import socket
    import struct
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(10)
    sock.connect(str(find_socket()))
    payload = json.dumps({"version": 3, **command}).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)
    header = sock.recv(4, socket.MSG_WAITALL)
    reply = b""
    if len(header) == 4:
        want = struct.unpack(">I", header)[0]
        while len(reply) < want:
            chunk = sock.recv(want - len(reply))
            if not chunk:
                break
            reply += chunk
    sock.close()
    return reply.decode(errors="replace")


def ipc_cancel(run_id):
    return ipc_request({"command": "cancel", "run_id": run_id})


def jobs_overview():
    try:
        return json.loads(ipc_request({"command": "jobs_overview"})).get("data", {}).get("jobs", [])
    except (json.JSONDecodeError, AttributeError):
        return []


def leftovers(marker):
    out = subprocess.run(["pgrep", "-af", marker], capture_output=True, text=True).stdout.splitlines()
    return [line for line in out if not line.startswith(str(os.getpid()) + " ") and "pgrep" not in line]


def main():
    assert BIN.exists(), f"missing binary {BIN}"
    build_home()
    stub = subprocess.Popen([sys.executable, str(BASE / "stub.py")],
                            env=dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_LOG=str(STUB_LOG)),
                            stdout=(OUT / "stub.log").open("w"), stderr=subprocess.STDOUT)
    daemon = None
    try:
        time.sleep(0.5)
        daemon = start_daemon()

        # 1. 子代理(09-26 起只在后台跑):派完这一轮就收,结论由汇报叫醒的那一轮带回来;
        #    一次性 JSON 输出等整棵子树跑完,终态就是那一轮的结论。
        done = ask("main", "TK fg-plain", create=True)
        text = reply_text(done)
        check("child_result_reaches_parent", "WOKEN" in text and "CHILD_RESULT ok" in text, text[:160])
        main_row = session_by_name("main")
        assert main_row, "main session missing"
        main_id = main_row["session_id"]
        child = child_by_directive(main_id, "CHILD plain")
        child_turns = turns_of(child["session_id"]) if child else []
        check("child_session_rows",
              child and child["kind"] == "subagent" and child["depth"] == 1 and child["task_state"] == "done"
              and child["background"] == 1 and len(child_turns) == 1 and child_turns[0]["status"] == "completed",
              {k: child.get(k) for k in ("kind", "depth", "task_state", "background")} if child else None)
        # 派它的那一轮(不一定是最后一轮:后面还有汇报叫醒的那一轮)的 tool_flow 里落了子会话 id。
        flow = " ".join(turn["tool_flow"] or "" for turn in turns_of(main_id))
        check("parent_turn_links_child_session", child and child["session_id"] in flow, flow[:200])

        # 2. 孙代理(前台)
        done = ask("main", "TK fg-gc")
        text = reply_text(done)
        gc_child = child_by_directive(main_id, "CHILD spawn-gc")
        grandchild = child_by_directive(gc_child["session_id"], "GRANDCHILD plain") if gc_child else None
        check("grandchild_rows",
              "GC_RESULT ok" in text and gc_child and gc_child["task_state"] == "done"
              and grandchild and grandchild["depth"] == 2 and grandchild["task_state"] == "done",
              {"reply": text[:80], "gc": grandchild and grandchild["depth"]})

        # 3. 孙代理面上没有 subagent
        done = ask("main", "TK gc-depth")
        text = reply_text(done)
        gc_requests = [r for r in stub_records() if r["role"] == "subagent" and r["directive"].startswith("GRANDCHILD try-spawn")]
        child_requests = [r for r in stub_records() if r["role"] == "subagent" and r["directive"].startswith("CHILD ")]
        check("grandchild_has_no_subagent_tool",
              "GC_NO_SUBAGENT_TOOL" in text and gc_requests and all("subagent" not in r["tools"] for r in gc_requests)
              and child_requests and all("subagent" in r["tools"] for r in child_requests),
              {"reply": text[:60], "gc_tools_have_subagent": [("subagent" in r["tools"]) for r in gc_requests]})

        # 4. 子代理等自己的后台命令
        started = time.time()
        done = ask("main", "TK fg-bgcmd")
        elapsed = time.time() - started
        text = reply_text(done)
        bg_child = child_by_directive(main_id, "CHILD run-bg-cmd")
        bg_turns = turns_of(bg_child["session_id"]) if bg_child else []
        check("child_waits_for_background_command",
              "CHILD_AFTER_BG done" in text and "CHILD_STARTED_BG" not in text.split("result:")[-1][:40]
              and bg_child and bg_child["task_state"] == "done" and len(bg_turns) == 2 and elapsed >= 3,
              {"reply": text[:80], "turns": len(bg_turns), "elapsed": round(elapsed, 1)})

        # 5. 后台孙代理唤醒的是子代理:主会话只收到子代理那一份汇报,孙代理的汇报不直达主会话
        def parent_reports():
            return [t["user_content"] for t in turns_of(main_id) if t["user_content"].startswith("<background-job-report>")]
        reports_before = len(parent_reports())
        done = ask("main", "TK fg-gc-bg")
        text = reply_text(done)
        gcbg_child = child_by_directive(main_id, "CHILD spawn-gc-bg")
        gcbg = child_by_directive(gcbg_child["session_id"], "GRANDCHILD plain") if gcbg_child else None
        new_reports = parent_reports()[reports_before:]
        # 孙代理跑得快时唤醒直接排进子代理还在跑的那一轮(1 轮);慢则另起一轮(2 轮)。
        child_turn_count = len(turns_of(gcbg_child["session_id"])) if gcbg_child else 0
        check("grandchild_background_wakes_child_not_parent",
              "CHILD_AFTER_GC done" in text and gcbg and gcbg["background"] == 1 and gcbg["task_state"] == "done"
              and gcbg_child["task_state"] == "done" and child_turn_count in (1, 2)
              and len(new_reports) <= 1 and not any("bg grandchild" in report for report in new_reports),
              {"after_gc": "CHILD_AFTER_GC done" in text, "child_turns": child_turn_count,
               "gc_bg": gcbg and (gcbg["background"], gcbg["task_state"]),
               "parent_reports": [report[:60] for report in new_reports]})

        # 6. 后台子代理唤醒主会话
        done = ask("main", "TK bg-plain")
        text = reply_text(done)
        woke = wait_for(lambda: next((t for t in turns_of(main_id)
                                      if t["user_content"].startswith("<background-job-report>")
                                      and t["status"] == "completed" and "WOKEN" in (t["assistant_content"] or "")), None), 40)
        # 子代理跑得比父回合收尾还快时,唤醒直接排进父回合当 follow-up,最终正文就是
        # WOKEN;慢的话父回合先结束,之后另起一轮合成轮。两种都算对。
        inline = "WOKEN" in text and "CHILD_RESULT ok" in text
        check("background_child_wakes_parent_with_report",
              inline or (woke and "CHILD_RESULT ok" in woke["assistant_content"]),
              {"inline": inline, "reply": text[:60], "woken": (woke or {}).get("assistant_content", "")[:120]})

        # 7. 一次性命令等子代理时按 Ctrl+C:整棵子树停下,子会话那一轮标中断,命令按取消退出(130)。
        # (主回合还在跑时停它、连它这一轮派出去的子代理一起停,Rust 那边有单测。)
        proc = subprocess.Popen([str(BIN), "ask", "--output-format", "stream-json", "--session", "main", "TK fg-slow"],
                                env=env(), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        slow_child = wait_for(lambda: child_by_directive(main_id, "CHILD slow"), 30)
        running = wait_for(lambda: any(t["status"] == "running" for t in turns_of(slow_child["session_id"])), 20) if slow_child else None
        sleeping = wait_for(lambda: leftovers("sleep 40.5"), 10)
        proc.send_signal(signal.SIGINT)
        try:
            code = proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            code = None
        interrupted = wait_for(lambda: (lambda row: row and row["task_state"] == "interrupted")(child_by_directive(main_id, "CHILD slow")), 20)
        slow_turns = turns_of(slow_child["session_id"]) if slow_child else []
        gone = wait_for(lambda: not leftovers("sleep 40.5"), 15)
        check("ctrl_c_while_waiting_stops_the_subtree",
              slow_child and running and sleeping and code == 130 and interrupted
              and slow_turns and slow_turns[-1]["status"] == "interrupted" and gone,
              {"exit": code, "running_seen": bool(running), "sleep_seen": bool(sleeping),
               "task_state": (child_by_directive(main_id, "CHILD slow") or {}).get("task_state"),
               "turn_status": slow_turns[-1]["status"] if slow_turns else None, "leftover_gone": bool(gone)})

        # 7b. 开发模式会话开的后台子代理(没传 dev)也是开发模式:镜像任务的标签按实际来,
        #     子会话人格是 dev(用户 09-18:任务条上孙代理写成「子代理」)。
        # 一次性命令会一直等到子树跑完(子代理那条后台命令要睡 60 秒):放在后台跑,看完就 Ctrl+C。
        dev_proc = spawn_ask("devmain", "TK bg-wait", create=True, mode="dev")
        dev_row = wait_for(lambda: session_by_name("devmain"), 30)
        dev_waiting = wait_for(lambda: (lambda row: row and row["task_state"] == "waiting")(child_by_directive(dev_row["session_id"], "CHILD run-bg-cmd-long")), 40) if dev_row else None
        dev_child = child_by_directive(dev_row["session_id"], "CHILD run-bg-cmd-long") if dev_row and dev_waiting else None
        dev_jobs = [j for j in jobs_overview() if dev_row and j.get("session_id") == dev_row["session_id"]]
        check("dev_parent_child_labelled_dev",
              dev_child and dev_child["persona"] == "dev" and dev_jobs and all(j.get("kind") == "dev" for j in dev_jobs),
              {"persona": dev_child and dev_child["persona"], "kinds": [j.get("kind") for j in dev_jobs]})
        interrupt(dev_proc)

        # 8. 重启把 waiting 标 interrupted,回复即续。一次性命令等着子树:它挂着的时候终端被关掉
        #    (SIGKILL,来不及停子代理),子代理照旧在 daemon 里等它的后台命令。
        wait_proc = spawn_ask("main", "TK bg-wait")
        waiting = wait_for(lambda: (lambda row: row and row["task_state"] == "waiting")(child_by_directive(main_id, "CHILD run-bg-cmd-long")), 40)
        # 后代的后台任务带树根:子代理开的那条 sleep 60 归子会话,root 是主会话,主会话的
        # 任务条据此把它列出来(用户 09-18:主会话里看不见孙代理)。
        wait_child_row = child_by_directive(main_id, "CHILD run-bg-cmd-long")
        jobs = jobs_overview()
        nested = [j for j in jobs if wait_child_row and j.get("session_id") == wait_child_row["session_id"]]
        check("descendant_jobs_carry_root_session",
              nested and all(j.get("root_session_id") == main_id for j in nested)
              and all(j.get("root_session_id") == main_id for j in jobs if j.get("session_id") == main_id),
              {"jobs": [(j.get("kind"), j.get("session_id", "")[:20], j.get("root_session_id", "")[:20]) for j in jobs]})
        wait_proc.kill()
        wait_proc.wait()
        stop_daemon(daemon)
        daemon = start_daemon()
        wait_child = child_by_directive(main_id, "CHILD run-bg-cmd-long")
        marked = wait_child and wait_child["task_state"] == "interrupted"
        resumed = ask(wait_child["session_id"], "TK continue") if wait_child else {}
        after = child_by_directive(main_id, "CHILD run-bg-cmd-long")
        check("restart_marks_interrupted_and_reply_resumes",
              waiting and marked and "CHILD_CONTINUED" in reply_text(resumed) and after and after["task_state"] == "done",
              {"waiting_seen": bool(waiting), "marked": bool(marked), "reply": reply_text(resumed)[:60],
               "after": after and after["task_state"]})

        # 9. 用量汇总:主会话累计 = 整棵树 turns 之和(桩每次请求 50 词元)
        tree_ids = [main_id]
        frontier = [main_id]
        while frontier:
            nxt = []
            for sid in frontier:
                for row in children_of(sid):
                    if row["task_state"] is not None:
                        tree_ids.append(row["session_id"])
                        nxt.append(row["session_id"])
            frontier = nxt
        expected = sum(t["token_total"] or 0 for sid in tree_ids for t in turns_of(sid))
        own = sum(t["token_total"] or 0 for t in turns_of(main_id))
        code, out, _ = cli(["session", "show", "--json", "main"])
        shown = None
        try:
            payload = json.loads(out) if code == 0 and out.strip() else {}
            shown = payload.get("cumulative_tokens")
        except json.JSONDecodeError:
            shown = None
        check("tokens_rollup_recursive", shown is not None and shown == expected and expected > own,
              {"shown": shown, "expected_tree": expected, "own_only": own, "sessions": len(tree_ids)})

        # 10. 删主会话连树删
        ask("tree", "TK fg-gc", create=True)
        tree = session_by_name("tree")
        tree_children = [row["session_id"] for row in children_of(tree["session_id"])] if tree else []
        grand_ids = [g["session_id"] for c in tree_children for g in children_of(c)]
        code, _, err = cli(["session", "delete", "--yes", "tree"])
        remaining = query("SELECT session_id FROM sessions WHERE session_id IN ({})".format(
            ",".join("?" * (len(tree_children) + len(grand_ids)))), tuple(tree_children + grand_ids)) if tree_children else [1]
        check("delete_cascades_tree", code == 0 and tree_children and grand_ids and not remaining,
              {"code": code, "children": len(tree_children), "grandchildren": len(grand_ids), "remaining": len(remaining), "err": err.strip()[:80]})
    finally:
        if daemon is not None:
            stop_daemon(daemon)
        stub.terminate()
        time.sleep(1.0)
        left = leftovers("miyu-subagent-test")
        check("no_orphans", not left, left)
        (OUT / "verdict.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), "utf-8")
        print("ask seconds:", ASK_SECONDS)
    passed = sum(results.values())
    print(f"\n{passed}/{len(results)} passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()

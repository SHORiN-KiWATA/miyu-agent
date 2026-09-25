#!/usr/bin/env python3
"""opencodego(deepseek-v4.1-flash)真机复杂场景缓存测试(09-25)。

隔离家目录(~/.cache/miyu-cache-live/<标签>/home)+ 18891–18899 里第一个空端口的 daemon。
供应商从本机 ~/.miyu/config/config.jsonc 只读地抄 opencodego 这一条(含 key),记忆开着
(本地 bge 向量模型链进沙箱),上下文与工具设置照抄用户的;请求录制打开,完整请求体留作二分断点用。
家目录里有 key:不论成败都删;产物(不含 key)留在 ~/.cache/miyu-cache-live/<标签>/ 下。

普通模式会话 main(记忆开着)一轮一个场景,轮与轮之间停不同的秒数(量跨轮对空闲时长的敏感度):

    t01 first        一句话,首请求
    t02 seq_tools    三条命令依次跑,输出从几行到一千多行
    t03 parallel     同一次回复里并行三个 read + 一个命令
    t04 fg_subagent  前台子代理
    t05 bg_subagent  后台子代理 → 等它跑完、主会话被唤醒那一轮也跑完
    t06 followup     第一条命令跑着时插一句(并进这一轮)
    t07 interrupt    第二条命令跑着时按停止
    t08 after_stop   接着做完
    t09 skill        往沙箱里放一件新技能之后的一轮
    ——   /compact     IPC 压缩(fork 式摘要)
    t10 post_compact 压缩后的工具轮
    t11 wrap         一句话收尾

开发模式会话 dev:看目录 → 改代码加测试并跑 → 改 README → 再跑测试。

用法: opencodego_live.py <miyu 二进制> <标签> [--skip-dev] [--gaps 2,5,20,45,90,180]
"""

import argparse
import json
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import hit_breakdown  # noqa: E402
from replay_live import Sandbox, recv_exact, wait_for  # noqa: E402

RUN_ROOT = Path.home() / ".cache/miyu-cache-live"
USER_CONFIG = Path.home() / ".miyu/config/config.jsonc"
USER_MODELS_CACHE = Path.home() / ".miyu/cache/models_cache.json"
EMBED_MODEL = "bge-small-zh-v1.5-int8"
# 仓库里的本地 embedding 模型（`assets/models/`），沙箱家目录里放一份，记忆检索才开得起来。
EMBED_SOURCE = Path(__file__).resolve().parents[2] / "assets" / "models" / EMBED_MODEL
PORTS = range(18891, 18900)
PROVIDER_ID = "opencodego"
MODEL = "deepseek-v4.1-flash"


def user_config():
    text = USER_CONFIG.read_text(encoding="utf-8")
    text = re.sub(r"(?m)^\s*//.*$", "", text)
    text = re.sub(r",(\s*[}\]])", r"\1", text)
    return json.loads(text)


def port_free(port):
    for host in ("0.0.0.0", "127.0.0.1"):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                probe.bind((host, port))
            except OSError:
                return False
    return True


class GoSandbox(Sandbox):
    """replay_live.Sandbox 的 IPC / ask / 查库照用;家目录、端口、配置换成这次的。"""

    def __init__(self, miyu, label):  # noqa: D401 — 不调父类构造:它会写 deepseek 配置
        self.miyu = miyu
        self.run_dir = RUN_ROOT / label
        self.home = self.run_dir / "home"
        if self.home.exists():
            shutil.rmtree(self.home)
        self.port = next((port for port in PORTS if port_free(port)), None)
        if self.port is None:
            raise SystemExit("18891–18899 没有空端口")
        self.run = self.home / "run"
        self.work = self.home / "work"
        for path in (self.run, self.home / "config", self.work, self.home / "cache",
                     self.home / "models", self.home / "extensions" / "skills"):
            path.mkdir(parents=True, exist_ok=True)
        source = user_config()
        provider = next(p for p in source.get("providers", []) if p.get("id") == PROVIDER_ID)
        config = {
            "config_version": source.get("config_version", 3),
            "oobe_done": True,
            "active_provider": PROVIDER_ID,
            "active_provider_models": [{"provider_id": PROVIDER_ID, "model": MODEL}],
            "providers": [dict(provider)],
            "memory": {"enabled": True},
            "embedding": source.get("embedding") or {"enabled": True, "backend": "local", "local_model": EMBED_MODEL},
            "context": source.get("context") or {},
            "tools": source.get("tools") or {},
            "skills": {"enabled": True, "allow_command_execution": True},
            "voice": {"enabled": False},
        }
        (self.home / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
        (self.home / "models" / EMBED_MODEL).symlink_to(EMBED_SOURCE)
        if USER_MODELS_CACHE.exists():
            shutil.copyfile(USER_MODELS_CACHE, self.home / "cache" / "models_cache.json")
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith("HERDR_")
                    and k not in ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "MIYU_HOME",
                                  "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME")}
        self.env.update(MIYU_HOME=str(self.home), XDG_RUNTIME_DIR=str(self.run), LANG="zh_CN.UTF-8",
                        MIYU_LOG="info")
        self.daemon = None
        self.clients = []
        self.timeline = []

    # ---- 记事 ----
    def mark(self, event, **fields):
        entry = {"t": time.strftime("%H:%M:%S"), "event": event, **fields}
        self.timeline.append(entry)
        print(json.dumps(entry, ensure_ascii=False), flush=True)

    def ipc_long(self, command, timeout=600):
        sock_path = next(iter(self.run.rglob("*.sock")), None)
        payload = json.dumps({"version": 3, **command}).encode()
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(timeout)
            sock.connect(str(sock_path))
            sock.sendall(struct.pack(">I", len(payload)) + payload)
            (length,) = struct.unpack(">I", recv_exact(sock, 4))
            return json.loads(recv_exact(sock, length))

    def turn(self, session, name, text, timeout=900, **kwargs):
        started = time.time()
        self.mark("turn.start", name=name, session=session)
        try:
            self.ask(session, text, timeout=timeout, **kwargs)
            ok = True
        except (AssertionError, subprocess.TimeoutExpired) as error:
            ok = False
            self.mark("turn.error", name=name, error=str(error)[-300:])
        self.mark("turn.end", name=name, ok=ok, seconds=round(time.time() - started, 1))
        return ok

    def running(self, session_id):
        return any(t["status"] == "running" for t in self.turns(session_id))

    def settle(self, session_id, timeout=600):
        """回合、排队的插话、后台唤醒全部跑完。"""
        return wait_for(lambda: not self.running(session_id), timeout, step=1.0)

    def pids_in_home(self):
        pids = []
        for proc in Path("/proc").iterdir():
            if not proc.name.isdigit():
                continue
            try:
                environ = (proc / "environ").read_bytes()
            except OSError:
                continue
            if f"MIYU_HOME={self.home}".encode() in environ.split(b"\0"):
                pids.append(int(proc.name))
        return pids

    def stop(self):
        super().stop()
        for pid in self.pids_in_home():
            if pid == os.getpid():
                continue
            try:
                os.kill(pid, signal.SIGTERM)
            except OSError:
                pass
        time.sleep(1)
        for pid in self.pids_in_home():
            if pid != os.getpid():
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass


# ---------------------------------------------------------------- 场景用的文件

def write_files(work):
    lines = []
    for i in range(2600):
        level = "ERROR" if i % 17 == 0 else "WARN" if i % 5 == 0 else "INFO"
        lines.append(f"2026-09-25T10:{(i // 60) % 60:02d}:{i % 60:02d} {level} worker={i % 9} "
                     f"job=j{i * 7919 % 10007:05d} took={(i * 37) % 997}ms msg=step {i} finished")
    (work / "big.log").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (work / "mid.txt").write_text("\n".join(f"line {i}: the quick brown fox #{i * 13 % 101}" for i in range(420)),
                                  encoding="utf-8")
    (work / "small.txt").write_text("\n".join(f"item {i} = {i * i}" for i in range(30)), encoding="utf-8")
    rows = ["id,alpha,beta,gamma,delta"] + [f"{i},{i * 3 % 97},{i * 7 % 89},{i * 11 % 83},{i * 13 % 79}"
                                            for i in range(900)]
    (work / "data.csv").write_text("\n".join(rows) + "\n", encoding="utf-8")
    proj = work / "proj"
    proj.mkdir(exist_ok=True)
    (proj / "calc.py").write_text(
        '"""Tiny calculator used by the cache walkthrough."""\n\n\n'
        "def add(a, b):\n    return a + b\n\n\n"
        "def sub(a, b):\n    return a - b\n\n\n"
        "def mul(a, b):\n    return a * b\n", encoding="utf-8")
    (proj / "test_calc.py").write_text(
        "import unittest\n\nfrom calc import add, mul, sub\n\n\n"
        "class CalcTest(unittest.TestCase):\n"
        "    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\n"
        "    def test_sub(self):\n        self.assertEqual(sub(5, 3), 2)\n\n"
        "    def test_mul(self):\n        self.assertEqual(mul(4, 3), 12)\n\n\n"
        "if __name__ == '__main__':\n    unittest.main()\n", encoding="utf-8")
    (proj / "README.md").write_text("# calc\n\nAdd, subtract and multiply two numbers.\n", encoding="utf-8")


def write_skill(home, name, body):
    skill = home / "extensions" / "skills" / name
    skill.mkdir(parents=True, exist_ok=True)
    (skill / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: {body}\n---\n\n"
        "When this skill is loaded, answer in three bullet points: finding, evidence, next step.\n",
        encoding="utf-8")


# ---------------------------------------------------------------- 剧本

T02 = ("请依次用 run_command 执行下面三条命令,每条等上一条的结果出来再执行下一条:"
       "`wc -l big.log`、`head -n 400 big.log`、`seq 1 3000 | awk '{print $1*$1}' | tail -n 1500`。"
       "三条都跑完后用一句话总结。")
T03 = ("请在同一次回复里并行发起四个工具调用(不要一个一个来):用 read 读 mid.txt、用 read 读 small.txt、"
       "用 read 读 data.csv 的前 300 行、用 run_command 执行 `ls -la`。四个结果都拿到后用两句话说说这几个文件分别是什么。")
T04 = ("用 subagent 工具派一个前台子代理(不要后台):让它统计 big.log 里 ERROR 和 WARN 各有多少行,"
       "再找出 ERROR 最多的 worker。它的 prompt 里要写清楚文件在当前工作目录。拿到它的结论后用一句话告诉我。")
T05 = ("用 subagent 工具派一个后台子代理(background=true):让它读 data.csv,算出 alpha/beta/gamma/delta 四列各自的"
       "最小值和最大值,prompt 里写清楚文件在当前工作目录。派完直接回我一句「已派出」,不用等它。")
T06 = ("请依次用 run_command 执行三条命令,每条等上一条结果出来再执行下一条:"
       "`echo F1 >> counter.txt; sleep 8; seq 1 600`、`echo F2 >> counter.txt; seq 601 1200`、"
       "`echo F3 >> counter.txt; seq 1201 1800`。最后用一句话总结。")
T06_FOLLOWUP = "另外,总结时顺便说一下这三段一共输出了多少行。"
T07 = ("请依次用 run_command 执行三条命令,每条等上一条结果出来再执行下一条:"
       "`echo I1 >> counter.txt; seq 1 500`、`echo I2 >> counter.txt; sleep 60; seq 501 1000`、"
       "`echo I3 >> counter.txt; seq 1001 1500`。最后用一句话总结。")
T08 = "刚才那轮被我按停了。把没跑完的命令接着跑完(已经跑过的别重跑),然后用一句话总结。"
T09 = "我刚在技能目录里加了一件新技能。看看你现在能用的技能有哪些,挑刚加的那件说说它是干什么的,一两句话。"
T10 = "请用 run_command 执行 `tail -n 200 big.log`,然后用一句话说说最后这些行里 ERROR 占多少。"
T11 = "好的,最后用一句话总结我们这次一共做了哪些事。"

D01 = "看一下 proj/ 目录的结构,再读一下 calc.py 和 test_calc.py,简短说说这个项目。"
D02 = ("给 proj/calc.py 加一个 divide(a, b) 函数:b 为 0 时抛 ValueError。再在 proj/test_calc.py 里补两条测试"
       "(正常相除、除数为 0),然后在 proj 目录下跑 `python3 -m unittest -q` 确认全过。")
D03 = "把 proj/README.md 更新一下,写上新加的 divide 函数和它对 0 的处理。"
D04 = "在 proj 目录下再跑一次 `python3 -m unittest -q`,告诉我结果。"


def gap(box, seconds, before):
    box.mark("idle", seconds=seconds, before=before)
    time.sleep(seconds)


def run_main(box, gaps):
    gaps = list(gaps)

    def next_gap():
        value = gaps.pop(0)
        gaps.append(value)
        return value

    box.turn("main", "t01_first", "你好,用一句话介绍一下你自己。", create=True)
    main = box.session_id("main")
    box.mark("session", role="main", id=main)
    for name, text in (("t02_seq_tools", T02), ("t03_parallel", T03), ("t04_fg_subagent", T04)):
        gap(box, next_gap(), name)
        box.turn(main, name, text)
        box.settle(main)

    gap(box, next_gap(), "t05_bg_subagent")
    before = len(box.turns(main))
    box.turn(main, "t05_bg_subagent", T05)
    # 后台子代理跑完 → 主会话被唤醒那一轮 → 跑完。
    woke = wait_for(lambda: len(box.turns(main)) > before + 1 and not box.running(main), 600, step=2.0)
    box.mark("bg_wake", woke=bool(woke), turns=len(box.turns(main)))
    box.settle(main)

    gap(box, next_gap(), "t06_followup")
    box.mark("turn.start", name="t06_followup", session=main, background=True)
    box.ask_in_background(main, T06)
    if wait_for(lambda: box.counter("F1") > 0, 300):
        time.sleep(2)
        box.mark("followup.sent")
        box.ask_in_background(main, T06_FOLLOWUP)
    else:
        box.mark("followup.skipped", reason="F1 never started")
    wait_for(lambda: box.counter("F3") > 0, 400)
    box.settle(main)
    box.mark("turn.end", name="t06_followup")

    gap(box, next_gap(), "t07_interrupt")
    box.mark("turn.start", name="t07_interrupt", session=main, background=True)
    box.ask_in_background(main, T07)
    if wait_for(lambda: box.counter("I2") > 0, 300):
        time.sleep(3)
        stopped = box.stop_running(main)
        box.mark("interrupt.sent", runs=len(stopped))
    else:
        box.mark("interrupt.skipped", reason="I2 never started")
    wait_for(lambda: not box.running(main), 60)
    box.mark("turn.end", name="t07_interrupt")

    gap(box, next_gap(), "t08_after_stop")
    box.turn(main, "t08_after_stop", T08)
    box.settle(main)

    write_skill(box.home, "incident-brief", "Write a three-point incident brief from logs. Use when the user asks "
                "for an incident summary or 事故简报.")
    box.mark("skill.added", name="incident-brief")
    gap(box, next_gap(), "t09_skill")
    box.turn(main, "t09_skill", T09)
    box.settle(main)

    gap(box, next_gap(), "compact")
    box.mark("compact.start")
    try:
        result = box.ipc_long({"command": "compact", "target": {"kind": "id", "id": main}}, timeout=600)
        box.mark("compact.end", ok=bool(result.get("ok", result.get("status") != "error")),
                 result=json.dumps(result, ensure_ascii=False)[:300])
    except OSError as error:
        box.mark("compact.error", error=str(error))

    gap(box, next_gap(), "t10_post_compact")
    box.turn(main, "t10_post_compact", T10)
    box.settle(main)
    gap(box, next_gap(), "t11_wrap")
    box.turn(main, "t11_wrap", T11)
    box.settle(main)
    return main


def run_dev(box, gaps):
    gaps = list(gaps)
    box.turn("dev", "d01_look", D01, create=True, mode="dev")
    dev = box.session_id("dev")
    box.mark("session", role="dev", id=dev)
    for index, (name, text) in enumerate((("d02_edit_test", D02), ("d03_readme", D03), ("d04_retest", D04))):
        gap(box, gaps[index % len(gaps)], name)
        box.turn(dev, name, text)
        box.settle(dev)
    return dev


# ---------------------------------------------------------------- 收尾与分析

def collect(box, sessions):
    out = box.run_dir
    logs = out / "logs"
    logs.mkdir(exist_ok=True)
    for path in box.home.rglob("cache-usage.*.jsonl"):
        shutil.copyfile(path, logs / path.name)
    for path in box.home.rglob("requests-*.jsonl"):
        shutil.copyfile(path, logs / path.name)
    rows = box.query("SELECT session_id, kind, parent_session_id, name, task_state, background, created_at "
                     "FROM sessions ORDER BY created_at")
    turns = box.query("SELECT session_id, seq, status, hidden, is_summary, user_timestamp, assistant_timestamp, "
                      "length(user_content) AS ulen, token_prompt, token_cache_read FROM turns ORDER BY session_id, seq")
    queued = box.query("SELECT session_id, status, count(*) AS n FROM queued_prompts GROUP BY session_id, status")
    summary = {"sessions": rows, "turns": turns, "queued": queued, "timeline": box.timeline,
               "named": sessions, "port": box.port}
    (out / "db_summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=1), encoding="utf-8")
    return summary


def roles_for(summary):
    roles = {}
    names = {v: k for k, v in summary["named"].items() if v}
    for row in summary["sessions"]:
        sid = row["session_id"]
        if sid in names:
            roles[sid] = names[sid]
        elif row["kind"] == "subagent":
            parent_role = names.get(row["parent_session_id"], "?")
            roles[sid] = f"sub({parent_role}):{(row['name'] or '')[:10]}"
    return roles


def analyze(run_dir, summary):
    files = sorted(str(p) for p in (run_dir / "logs").glob("cache-usage.*.jsonl"))
    if not files:
        print("没有 cache-usage 日志")
        return
    roles = roles_for(summary)
    args = files + [f"--sess={sid}={role}" for sid, role in roles.items()]
    args += ["--json", str(run_dir / "breakdown.json"), "--rows"]
    print("\n===== 会话内请求(chat) =====")
    hit_breakdown.main(args + ["--scope", "chat"])
    rows = hit_breakdown.load(files)
    total_prompt = sum(r.get("prompt") or 0 for r in rows)
    by_scope = {}
    for row in rows:
        scope = row.get("scope") or "?"
        entry = by_scope.setdefault(scope, [0, 0, 0])
        entry[0] += 1
        entry[1] += row.get("prompt") or 0
        entry[2] += row.get("cache_read") or 0
    print("\n===== 全部请求按 scope =====")
    for scope, (n, prompt, read) in sorted(by_scope.items()):
        print(f"  {scope:<22} n={n:>4} prompt={prompt:>9} read={read:>9} hit={100 * read / max(prompt, 1):6.2f}%")
    print(f"\n本次运行 prompt token 合计: {total_prompt}")
    (run_dir / "budget.json").write_text(json.dumps({"prompt_tokens": total_prompt, "by_scope": by_scope}),
                                         encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("miyu")
    parser.add_argument("label")
    parser.add_argument("--skip-dev", action="store_true")
    parser.add_argument("--skip-main", action="store_true")
    parser.add_argument("--gaps", default="2,5,20,45,90,180")
    parser.add_argument("--smoke", action="store_true", help="只跑首轮 + 一个工具轮,验管线")
    args = parser.parse_args()
    gaps = [int(x) for x in args.gaps.split(",")]
    box = GoSandbox(Path(args.miyu).resolve(), args.label)

    def on_term(*_):
        raise SystemExit("terminated")

    signal.signal(signal.SIGTERM, on_term)
    sessions = {}
    summary = None
    try:
        write_files(box.work)
        box.start()
        box.mark("daemon", port=box.port, pid=box.daemon.pid)
        logging = box.ipc({"command": "set_request_logging", "enabled": True})
        box.mark("request_logging", ok=json.dumps(logging)[:120])
        if args.smoke:
            box.turn("main", "t01_first", "你好,用一句话介绍一下你自己。", create=True)
            sessions["main"] = box.session_id("main")
            box.turn(sessions["main"], "t02_seq_tools", T02)
            box.settle(sessions["main"])
        elif not args.skip_main:
            sessions["main"] = run_main(box, gaps)
        if not args.skip_dev and not args.smoke:
            sessions["dev"] = run_dev(box, gaps)
    except BaseException as error:  # noqa: BLE001 — 任何中断都要走到收尾
        box.mark("aborted", error=repr(error)[:300])
    finally:
        try:
            summary = collect(box, sessions)
        except Exception as error:  # noqa: BLE001
            print(f"collect failed: {error!r}")
        box.stop()
        # 家目录里有 API key:不论成败都删。
        shutil.rmtree(box.home, ignore_errors=True)
        print(f"home removed: {not box.home.exists()}  leftover pids: {box.pids_in_home()}")
    if summary:
        analyze(box.run_dir, summary)
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""真模型长会话缓存套件(09-24):一整段开发模式会话从头跑到尾,看每种回合形态之后下一轮吃不吃得到缓存。

`replay_live.py` 是三个小会话的快检;这个是长会话。先把上下文堆起来(断一次的代价放大到看得见),
再依次过下面的场景,每个场景之后发一句短话当「下一轮」,记它的第一条请求:

    normal            几次工具调用,正常跑完
    interrupt_tool    倒数第二个工具还在跑时按停止
    followup_tool     第二个工具跑着时插一句(并进这一轮)
    repeat            同一条命令原样连调三次(复读轮)
    interrupt_stream  模型正在输出长文时按停止(在飞的请求被掐)
    restart           两轮之间重启 daemon(换进程)

每个场景都拿库核对它确实发生了(回合被打断、插话被并入、flow 里真有连续同参轮、daemon 真换了
pid),没发生的标出来,不算数。

两档规模:
    small   三轮工具输出热身到约 4 万上下文,六个场景各一次,每边约 150 万 prompt token
    heavy   三条大段日志把上下文堆到约 14 万,六个场景一圈 + 打断/插话/复读再一圈(上下文涨到
            二十多万),每边约 1000 万 prompt token

用法: replay_live_suite.py <miyu 二进制> <标签> [small|heavy]
      逐请求记账(不含正文与 key)存到 ~/.cache/miyu-cache-replay/suite-<标签>.json
"""

import json
import secrets
import shutil
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from replay_live import Sandbox, wait_for  # noqa: E402

OUT_DIR = Path.home() / ".cache/miyu-cache-replay"
LINES = 1200
ALL = ("normal", "interrupt_tool", "followup_tool", "repeat", "interrupt_stream", "restart")
KEY = ("interrupt_tool", "followup_tool", "repeat")
PROFILES = {
    "small": {"warmup_messages": 0, "warmup_tool_turns": 3, "calls": 4, "cycles": (ALL,)},
    # 单条命令行参数上限 128 KiB(MAX_ARG_STRLEN),一条日志 1700 行约 112 KiB、四万来 token。
    "heavy": {"warmup_messages": 3, "warmup_lines": 1700, "warmup_tool_turns": 0, "calls": 5,
              "cycles": (ALL, KEY)},
}
ESSAY_PROMPT = (
    "Without using any tools, write a detailed essay of about 1500 words on the history of the printing press."
)
NEXT = "Reply with the single word: ok."


def tool_prompt(marker, sleeps, lines=LINES):
    calls = [
        f"call {index}: `echo {marker}_{index} >> counter.txt; seq 1 {lines}; sleep {sleep}`"
        for index, sleep in enumerate(sleeps, 1)
    ]
    return (
        f"Use the run_command tool {len(sleeps)} times, one command per call, waiting for each result "
        "before the next call. " + " ".join(calls) + ". Then reply with one short sentence."
    )


def repeat_prompt(marker):
    return (
        "Use the run_command tool three times in a row with exactly the same command each time, one call per "
        f"step, and do not change the command in any way: `echo {marker} >> counter.txt; seq 1 800; sleep 1`. "
        "Then reply with one short sentence."
    )


def warmup_message(nonce, index, total, lines):
    """一大段日志文本。带本次运行的 nonce:新旧两边的内容各不相同,吃不到对方的缓存。"""
    rows = [f"Log chunk {index + 1}/{total} of run {nonce}. Keep it for later; reply with the single word: noted."]
    for i in range(lines):
        rows.append(f"{nonce}-{index}-{i:05d} worker={i % 7} step={(i * 37) % 1000} "
                    f"status=ok latency={(i * 13) % 97}ms")
    return "\n".join(rows)


class Suite:
    def __init__(self, miyu, profile):
        self.box = Sandbox(miyu)
        self.profile = profile
        self.nonce = secrets.token_hex(6)
        # 压缩会在中途把前缀断一次、搅乱测量;窗口给大(模型本身是 100 万),这一套不测压缩
        # (单测覆盖了压缩时的复读折叠)。
        path = self.box.home / "config" / "config.jsonc"
        config = json.loads(path.read_text(encoding="utf-8"))
        config["providers"][0]["model_context_window"] = {"deepseek-flash": 1_000_000}
        path.write_text(json.dumps(config), encoding="utf-8")
        self.session = None
        self.measures = []

    # —— 读库 ——
    def rows(self):
        return self.box.cache_rows(self.session)

    def turns(self):
        return self.box.query(
            "SELECT turn_id, status, tool_flow FROM turns WHERE session_id = ? ORDER BY seq", (self.session,)
        )

    def wait_idle(self, timeout=600):
        return wait_for(lambda: self.turns() and all(t["status"] != "running" for t in self.turns()), timeout)

    def consumed(self):
        rows = self.box.query(
            "SELECT count(*) AS n FROM queued_prompts WHERE session_id = ? AND status = 'consumed'", (self.session,)
        )
        return rows[0]["n"] if rows else 0

    def last_turn_repeats(self):
        turns = self.turns()
        if not turns:
            return False
        flow = json.loads(turns[-1]["tool_flow"] or "[]")
        signatures = [
            [(call.get("name"), call.get("arguments")) for call in round_.get("calls", [])]
            for round_ in flow
            if not round_.get("remote")
        ]
        return any(a and a == b for a, b in zip(signatures, signatures[1:]))

    def streaming_now(self):
        turns = self.turns()
        if not turns or turns[-1]["status"] != "running":
            return False
        rows = self.box.query(
            "SELECT count(*) AS n FROM turn_journal_events WHERE turn_id = ? AND kind = 'assistant_content'",
            (turns[-1]["turn_id"],),
        )
        return rows and rows[0]["n"] > 0

    # —— 量 ——
    def measure(self, name, happened, detail=""):
        """发一句短话当下一轮,记它的第一条请求。"""
        before = len(self.rows())
        self.box.ask(self.session, NEXT, timeout=900)
        rows = self.rows()
        if len(rows) <= before:
            self.measures.append({"scenario": name, "happened": happened, "error": "no request logged"})
            return
        first, previous = rows[before], rows[before - 1]
        prompt = first.get("prompt") or 0
        read = first.get("cache_read") or 0
        same_head = None
        if "sys" in first and "sys" in previous:
            same_head = (first.get("sys"), first.get("tools_hash")) == (previous.get("sys"), previous.get("tools_hash"))
        self.measures.append({
            "scenario": name,
            "happened": bool(happened),
            "detail": detail,
            "prev": first.get("prev"),
            "same": first.get("same"),
            "at": first.get("at"),
            "role": first.get("role"),
            "same_head": same_head,
            "prompt": prompt,
            "cache_read": read,
            "hit": round(100 * read / prompt, 1) if prompt else None,
            "previous_prompt": previous.get("prompt") or 0,
            "recomputed": max(0, (previous.get("prompt") or 0) - read),
        })

    # —— 场景 ——
    def scenario_normal(self, tag, calls):
        self.box.ask(self.session, tool_prompt(f"N{tag}", [1] * calls), timeout=900)
        self.measure(f"normal{tag}", True)

    def scenario_interrupt_tool(self, tag, calls):
        sleeps = [1] * calls
        sleeps[-2] = 120
        marker = f"I{tag}"
        self.box.ask_in_background(self.session, tool_prompt(marker, sleeps))
        started = wait_for(lambda: self.box.counter(f"{marker}_{calls - 1}") > 0, 600)
        time.sleep(1)
        self.box.stop_running(self.session)
        self.wait_idle()
        cut = self.turns()[-1]["status"] == "interrupted"
        self.measure(f"interrupt_tool{tag}", bool(started) and cut, f"cut at call {calls - 1}")

    def scenario_followup_tool(self, tag, calls):
        sleeps = [1] * calls
        sleeps[1] = 8
        marker = f"F{tag}"
        before = self.consumed()
        self.box.ask_in_background(self.session, tool_prompt(marker, sleeps))
        wait_for(lambda: self.box.counter(f"{marker}_2") > 0, 600)
        self.box.ask_in_background(self.session, "Also end your final sentence with the word banana.")
        wait_for(lambda: self.box.counter(f"{marker}_{calls}") > 0, 600)
        time.sleep(2)
        self.wait_idle()
        merged = self.consumed() > before
        self.measure(f"followup_tool{tag}", merged, f"merged={merged}")

    def scenario_repeat(self, tag, calls):
        self.box.ask(self.session, repeat_prompt(f"R{tag}"), timeout=900)
        repeated = self.last_turn_repeats()
        self.measure(f"repeat{tag}", repeated, f"consecutive identical rounds={repeated}")

    def scenario_interrupt_stream(self, tag, calls):
        self.box.ask_in_background(self.session, ESSAY_PROMPT)
        streaming = wait_for(self.streaming_now, 300, step=0.3)
        time.sleep(2)
        self.box.stop_running(self.session)
        self.wait_idle()
        cut = self.turns()[-1]["status"] == "interrupted"
        self.measure(f"interrupt_stream{tag}", bool(streaming) and cut, f"streamed={bool(streaming)}")

    def scenario_restart(self, tag, calls):
        box = self.box
        old_pid = box.daemon.pid
        box.daemon.terminate()
        box.daemon.wait(timeout=30)
        box.start()
        self.measure(f"restart{tag}", box.daemon.pid != old_pid, "daemon restarted")

    def run(self):
        box = self.box
        profile = PROFILES[self.profile]
        box.start()
        # 开发模式:用户说的正是「即使是开发模式」;dev 人格也不会像默认人格那样拒写长文。
        box.ask("long", "Reply with the single word: ready.", create=True, mode="dev")
        self.session = box.session_id("long")
        total = profile["warmup_messages"]
        for index in range(total):
            box.ask(self.session, warmup_message(self.nonce, index, total, profile["warmup_lines"]), timeout=900)
        for index in range(profile["warmup_tool_turns"]):
            box.ask(self.session, tool_prompt(f"W{index}", [1, 1, 1]), timeout=900)
        for cycle, scenarios in enumerate(profile["cycles"], 1):
            tag = "" if cycle == 1 else f"#{cycle}"
            for scenario in scenarios:
                getattr(self, f"scenario_{scenario}")(tag, profile["calls"])


def main():
    if len(sys.argv) not in (3, 4):
        print(__doc__)
        return 2
    profile = sys.argv[3] if len(sys.argv) == 4 else "small"
    if profile not in PROFILES:
        print(__doc__)
        return 2
    suite = Suite(Path(sys.argv[1]).resolve(), profile)
    label = sys.argv[2]
    try:
        suite.run()
    except Exception as error:  # noqa: BLE001 — 中途出错也把量到的留下
        suite.measures.append({"scenario": "aborted", "error": repr(error)})
    finally:
        rows = suite.rows() if suite.session else []
        suite.box.stop()
        # 家目录里有 key:不论成败都删。
        shutil.rmtree(suite.box.home, ignore_errors=True)
    total_prompt = sum(r.get("prompt") or 0 for r in rows)
    total_read = sum(r.get("cache_read") or 0 for r in rows)
    recomputed = sum(m.get("recomputed", 0) for m in suite.measures if m.get("happened"))
    report = {
        "label": label,
        "profile": profile,
        "measures": suite.measures,
        "session": {
            "requests": len(rows),
            "prompt": total_prompt,
            "cache_read": total_read,
            "hit": round(100 * total_read / total_prompt, 2) if total_prompt else None,
            "recomputed_after_measured_scenarios": recomputed,
        },
        "requests": rows,
    }
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    (OUT_DIR / f"suite-{label}.json").write_text(json.dumps(report, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"== {label}({profile})  会话 {len(rows)} 次请求  Σprompt {total_prompt:,}  Σcache_read {total_read:,}  "
          f"命中 {report['session']['hit']}%  场景后已发过却重算合计 {recomputed:,}")
    for m in suite.measures:
        if "error" in m:
            print(f"  {m['scenario']:<18} ERROR {m['error']}")
            continue
        verdict = "纯追加" if m["prev"] is not None and m["same"] is not None and m["same"] >= m["prev"] else (
            "换进程" if m["prev"] is None else f"第{m['at']}条改写({m['role']})")
        print(f"  {m['scenario']:<18} 发生={'是' if m['happened'] else '否'}  {verdict:<14} "
              f"下一轮首请求 {m['cache_read']:>7}/{m['prompt']:<7} = {m['hit']:>5}%  "
              f"已发过却重算 {m['recomputed']:>6}  开头指纹相同={m['same_head']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

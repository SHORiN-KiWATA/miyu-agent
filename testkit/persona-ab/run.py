#!/usr/bin/env python3
"""人格提示第二轮 A/B 测具。

驱动方式:PTY 里跑 `MIYU_DIRECT=1 miyu` 直连 REPL(不碰 daemon,绝不会连
QQ);回合完成不靠解析渲染输出,轮询该 home 的 conversation.db。**每个对话
一个独立 MIYU_HOME + 独立 REPL 进程**——默认会话即对话,零串扰,也绕开直连
REPL 不支持 /new 的限制(cli.rs 双 dispatch 分裂,见任务#14)。

测试配置从用户真实 config.jsonc 派生,但剥掉 platforms/web/voice 键、关
tools 与 memory,人格固定为内置默认 Miyu。

用法:
  python3 run.py smoke   # 冒烟:单对话两轮,验证全链路
  python3 run.py run     # 完整 2x2 矩阵
  python3 run.py score   # 用 results/*.jsonl 重跑打分
"""

import fcntl
import json
import os
import pty
import re
import shutil
import sqlite3
import struct
import subprocess
import sys
import termios
import threading
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
MIYU_BIN = REPO / "target" / "debug" / "miyu"
BASE = Path(__file__).resolve().parent
REAL_CONFIG = Path.home() / ".miyu" / "config" / "config.jsonc"
RESULTS = BASE / "results"

# ---------------------------------------------------------------- 配置派生

def strip_jsonc(text: str) -> str:
    """状态机剥 // 与 /* */ 注释,尊重字符串与转义(URL 里的 // 不受伤)。"""
    out = []
    i, n = 0, len(text)
    in_str = False
    while i < n:
        c = text[i]
        if in_str:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if c == '"':
            in_str = True
            out.append(c)
            i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            while i < n and text[i] != "\n":
                i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            i += 2
            while i + 1 < n and not (text[i] == "*" and text[i + 1] == "/"):
                i += 1
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def load_real_config() -> dict:
    raw = REAL_CONFIG.read_text(encoding="utf-8")
    return json.loads(strip_jsonc(raw), strict=False)


def build_home(tag: str, hint: bool, dialogs: bool, tools: bool = False) -> Path:
    """为一个对话准备干净的 MIYU_HOME。"""
    home = BASE / "homes" / tag
    if home.exists():
        shutil.rmtree(home)
    (home / "config").mkdir(parents=True)
    cfg = load_real_config()
    for key in ("platforms", "web", "voice", "alarm"):
        cfg.pop(key, None)
    cfg.setdefault("prompt", {})
    cfg["prompt"]["persona_reminder"] = hint
    cfg["prompt"]["active_persona"] = ""
    cfg.setdefault("tools", {})["enabled"] = tools
    cfg.setdefault("memory", {})["enabled"] = False
    (home / "config" / "config.jsonc").write_text(
        json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    if not dialogs:
        # 文件清空 = 显式停用内置预设对话(commit 71e48dc 约定)
        d = home / "config" / "prompts" / "dialogs"
        d.mkdir(parents=True)
        (d / "default.md").write_text("", encoding="utf-8")
    return home


# ---------------------------------------------------------------- PTY 驱动

class Repl:
    """PTY 里的直连 REPL。应答终端查询;回合完成靠 DB 轮询。"""

    def __init__(self, home: Path, log: Path, extra_env: dict | None = None):
        self.home = home
        self.db = home / "state" / "conversation.db"
        log.parent.mkdir(parents=True, exist_ok=True)
        self.log = open(log, "wb")
        env = dict(os.environ)
        env["MIYU_HOME"] = str(home)
        for key, value in (extra_env or {}).items():
            env[key] = value
        env["MIYU_DIRECT"] = "1"
        env["TERM"] = "xterm-256color"
        env.setdefault("LANG", "zh_CN.UTF-8")
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        # 裸 miyu 自②批起打印模式帮助退出;测具显式进普通模式。
        self.proc = subprocess.Popen(
            [str(MIYU_BIN)],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env,
            close_fds=True,
            preexec_fn=os.setsid,
        )
        os.close(slave)
        self._stop = False
        self.reader = threading.Thread(target=self._pump, daemon=True)
        self.reader.start()

    def _pump(self):
        """吞输出、答查询:ESC[6n→光标位置,ESC[c→DA1。"""
        buf = b""
        while not self._stop:
            try:
                chunk = os.read(self.master, 4096)
            except OSError:
                break
            if not chunk:
                break
            self.log.write(chunk)
            self.log.flush()
            buf = (buf + chunk)[-64:]
            if b"\x1b[6n" in buf:
                os.write(self.master, b"\x1b[1;1R")
                buf = buf.replace(b"\x1b[6n", b"")
            if b"\x1b[c" in buf or b"\x1b[0c" in buf:
                os.write(self.master, b"\x1b[?6c")
                buf = buf.replace(b"\x1b[c", b"").replace(b"\x1b[0c", b"")

    def send(self, line: str):
        os.write(self.master, line.encode("utf-8") + b"\r")

    def query(self, sql: str, args=()):
        if not self.db.exists():
            return []
        try:
            conn = sqlite3.connect(f"file:{self.db}?mode=ro", uri=True, timeout=5)
        except sqlite3.OperationalError:
            return []
        try:
            return conn.execute(sql, args).fetchall()
        except sqlite3.OperationalError:
            return []
        finally:
            conn.close()

    def wait(self, predicate, timeout: float, what: str):
        deadline = time.time() + timeout
        while time.time() < deadline:
            value = predicate()
            if value is not None:
                return value
            if self.proc.poll() is not None:
                raise RuntimeError(f"REPL 进程退出于等待 {what}")
            time.sleep(0.5)
        raise TimeoutError(f"等待 {what} 超时({timeout}s)")

    def visible_turns(self):
        return self.query(
            "SELECT seq, assistant_content, status FROM turns "
            "WHERE hidden = 0 AND is_summary = 0 ORDER BY seq"
        )

    def chat(self, text: str, timeout=240, retries=1) -> str:
        """发一轮,等 completed。interrupted/错误 → 重发一次。"""
        for attempt in range(retries + 1):
            before = len(self.visible_turns())
            self.send(text)

            def outcome():
                turns = self.visible_turns()
                if len(turns) <= before:
                    return None
                _, content, status = turns[-1]
                if status == "running":
                    return None
                return (content or "", status)

            content, status = self.wait(outcome, timeout, f"回合({text[:12]}…)")
            failed = status != "completed" or not content or content.startswith("错误")
            if not failed:
                return content
            if attempt < retries:
                time.sleep(3)
                continue
            raise RuntimeError(f"回合失败 status={status} content={content[:120]}")
        raise AssertionError("unreachable")

    def close(self):
        try:
            self.send("/exit")
            self.proc.wait(timeout=15)
        except Exception:
            try:
                self.proc.kill()
            except Exception:
                pass
        self._stop = True
        self.log.close()


def run_conversation(
    tag: str,
    hint: bool,
    dialogs: bool,
    turns: list[str],
    tools: bool = False,
    extra_env: dict | None = None,
) -> dict:
    """一个对话 = 独立 home + 独立 REPL 进程。返回全轮次问答与元数据。"""
    home = build_home(tag, hint, dialogs, tools)
    repl = Repl(home, RESULTS / f"{tag}.pty.log", extra_env)
    exchanges = []
    try:
        time.sleep(2.5)
        for text in turns:
            reply = repl.chat(text)
            reports = repl.query(
                "SELECT tool_flow FROM turns WHERE hidden = 0 ORDER BY seq DESC LIMIT 1"
            )
            used_tool = bool(reports and reports[0][0] and reports[0][0] not in ("", "[]", "null"))
            exchanges.append({"user": text, "assistant": reply, "used_tool": used_tool})
    finally:
        repl.close()
    return {"tag": tag, "hint": hint, "dialogs": dialogs, "exchanges": exchanges}


# ---------------------------------------------------------------- 矩阵

# 漂移压力填充轮:混入技术话题(长回复正式化正是人设漂移的主通道),全部
# 对话共用同一组,保证各探针承受同等压力。
FILLERS = [
    "最近在折腾一个 Rust 项目,给点动力",
    "解释一下什么是前缀缓存?",
    "那 KV cache 和它是什么关系?",
    "写代码的时候听什么音乐好?",
    "如果让你规划一个周末你会怎么安排?",
    "你觉得 AI 会取代程序员吗?",
]

# 深度加压组:14 个填充轮(技术向长回复是漂移主通道),探针在第 15 轮。
FILLERS_DEEP = FILLERS + [
    "讲讲 TCP 三次握手为什么是三次?",
    "那四次挥手又是为什么?",
    "Docker 和虚拟机的本质区别是什么?",
    "解释一下数据库索引为什么用 B+ 树?",
    "帮我理清 async/await 在 Rust 里的执行模型",
    "什么是零拷贝?",
    "聊聊你对微服务架构的看法",
    "内存泄漏和内存溢出有什么区别?",
]

# 闲聊探针(打分):最后一轮发出,只给这轮打分。
CHAT_PROBES = {
    "p1": "随便聊聊,今天心情一般",
    "p2": "你喜欢什么样的天气?",
    "p3": "给我讲个有意思的事",
    "p4": "你怎么看待拖延症?",
    "p5": "晚饭吃什么好?",
    "p6": "最近老失眠,有什么建议?",
}

# 技术答疑探针(只量字数,对齐 08-14 的"技术答疑中位字数"指标)。
QA_PROBES = {
    "q1": "解释一下 Rust 的所有权和借用检查是怎么工作的?",
    "q2": "SQLite 的 WAL 模式和普通 journal 有什么区别?",
}

VARIANTS = {
    "v1": {"hint": True, "dialogs": True},   # 现状
    "v2": {"hint": True, "dialogs": False},  # 只有 hint
    "v3": {"hint": False, "dialogs": True},  # 只有预设对话(用户假设/AstrBot 形态)
    "v4": {"hint": False, "dialogs": False}, # 裸基线
}

# 深度组只对比"现状"与"仅对话"——问题就是 hint 在长会话里是否仍不可少。
DEEP_VARIANTS = {
    "v1d": {"hint": True, "dialogs": True},
    "v3d": {"hint": False, "dialogs": True},
}

MATRIX = RESULTS / "matrix.jsonl"

# ---------------------------------------------------------------- 工具体制(style-lock A/B)

# 工具体制对话骨架:两轮漂移压力 → 一轮必然触发工具 → 闲聊探针打分。
# 打分只看工具轮之后的探针回复——测的是"工具循环会不会把人格洗成播报腔"。
# 触发语必须问知识截止后的事实,否则模型会凭记忆口答绕过工具(冒烟实测)。
TOOL_TRIGGERS = [
    "上网搜一下,今天有什么值得看的科技新闻?",
    "再搜搜 Arch Linux 官网最近一条新闻公告说了什么",
]
TOOL_FILLERS = FILLERS[:2]

TOOL_ARMS = {
    # A:无风格锁(MIYU_STYLE_LOCK=0);B:有风格锁(默认)。
    # 08-23 定稿:B 胜出(全过 5/12→8/12,无换行 6/12→10/12),style-lock 已硬
    # 编码进 prompt.rs 并拆除实验闸——现在重跑 tA 量到的和 tB 是同一件事,
    # 要复现对照需临时在 prompt.rs 恢复 MIYU_STYLE_LOCK 判断。
    "tA": {"style_lock": False},
    "tB": {"style_lock": True},
}

TOOL_MATRIX = RESULTS / "tool-matrix.jsonl"


def tool_existing_tags() -> set:
    if not TOOL_MATRIX.exists():
        return set()
    return {
        json.loads(line)["tag"]
        for line in TOOL_MATRIX.read_text(encoding="utf-8").splitlines()
        if line.strip()
    }


def run_tool_matrix():
    RESULTS.mkdir(exist_ok=True)
    done = tool_existing_tags()
    plan = []
    for aname, acfg in TOOL_ARMS.items():
        for pname, probe in CHAT_PROBES.items():
            tag = f"{aname}-{pname}"
            if tag not in done:
                plan.append((tag, acfg, probe))
    print(f"工具体制计划 {len(plan)} 个对话(已完成 {len(done)})", flush=True)
    for index, (tag, acfg, probe) in enumerate(plan, 1):
        turns = TOOL_FILLERS + TOOL_TRIGGERS + [probe]
        env = {"MIYU_STYLE_LOCK": "1" if acfg["style_lock"] else "0"}
        try:
            record = run_conversation(
                tag, hint=True, dialogs=True, turns=turns, tools=True, extra_env=env
            )
        except Exception as error:
            print(f"[{index}/{len(plan)}] {tag} 失败: {error}", flush=True)
            continue
        record["style_lock"] = acfg["style_lock"]
        with open(TOOL_MATRIX, "a", encoding="utf-8") as f:
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
        triggered = sum(1 for e in record["exchanges"][-3:-1] if e["used_tool"])
        final = record["exchanges"][-1]["assistant"]
        print(
            f"[{index}/{len(plan)}] {tag} ✓ 工具触发 {triggered}/2 探针回复 {len(final)} 字",
            flush=True,
        )
    print("工具矩阵完成", flush=True)


def score_tool_matrix():
    records = [
        json.loads(line)
        for line in TOOL_MATRIX.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    by_tag = {r["tag"]: r for r in records}
    print(f"{'臂':<3} {'lock':<5} {'触发对话':<8} {'探针全过':<8} {'各检查通过(只计触发对话)'}")
    for aname, acfg in TOOL_ARMS.items():
        passes, checks_total, n, triggered = 0, {}, 0, 0
        for pname in CHAT_PROBES:
            r = by_tag.get(f"{aname}-{pname}")
            if not r:
                continue
            n += 1
            # 只有工具真触发过的对话才进入风格统计,否则两臂比的不是同一件事
            if not any(e["used_tool"] for e in r["exchanges"][-3:-1]):
                continue
            triggered += 1
            reply = r["exchanges"][-1]["assistant"]
            checks = score_chat(reply)
            if all(checks.values()):
                passes += 1
            for k, ok in checks.items():
                checks_total[k] = checks_total.get(k, 0) + (1 if ok else 0)
        detail = " ".join(f"{k}:{v}/{triggered}" for k, v in checks_total.items())
        print(f"{aname:<3} {str(acfg['style_lock']):<5} {triggered}/{n:<6} {passes}/{triggered:<6} {detail}")


def existing_tags() -> set:
    if not MATRIX.exists():
        return set()
    tags = set()
    for line in MATRIX.read_text(encoding="utf-8").splitlines():
        if line.strip():
            tags.add(json.loads(line)["tag"])
    return tags


def run_matrix(deep=False):
    RESULTS.mkdir(exist_ok=True)
    done = existing_tags()
    variants = DEEP_VARIANTS if deep else VARIANTS
    probes = (
        list(CHAT_PROBES.items())
        if deep
        else list(CHAT_PROBES.items()) + list(QA_PROBES.items())
    )
    fillers = FILLERS_DEEP if deep else FILLERS
    plan = []
    for vname, vcfg in variants.items():
        for pname, probe in probes:
            tag = f"{vname}-{pname}"
            if tag not in done:
                plan.append((tag, vcfg, probe))
    print(f"计划 {len(plan)} 个对话(已完成 {len(done)})", flush=True)
    for index, (tag, vcfg, probe) in enumerate(plan, 1):
        turns = fillers + [probe]
        try:
            record = run_conversation(tag, vcfg["hint"], vcfg["dialogs"], turns)
        except Exception as error:
            print(f"[{index}/{len(plan)}] {tag} 失败: {error}", flush=True)
            continue
        with open(MATRIX, "a", encoding="utf-8") as f:
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
        final = record["exchanges"][-1]["assistant"]
        print(f"[{index}/{len(plan)}] {tag} ✓ 探针回复 {len(final)} 字", flush=True)
    print("矩阵完成", flush=True)


# ---------------------------------------------------------------- 打分

EMOJI = re.compile(
    "[\U0001f000-\U0001faff☀-➿️\U0001fb00-\U0001fbff]"
)


def score_chat(reply: str) -> dict:
    """默认 Miyu 人格显式规则的机械化子集(与 08-14 判据同源):
    ≤100 字、不换行、无表情符号、不用括号写动作(代理:无任何括号)。"""
    return {
        "len_le_100": len(reply) <= 100,
        "no_newline": "\n" not in reply,
        "no_emoji": not EMOJI.search(reply),
        "no_parens": not any(ch in reply for ch in "（("),
    }


def score_matrix():
    records = [
        json.loads(line)
        for line in MATRIX.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    by_tag = {r["tag"]: r for r in records}
    print(f"{'变体':<4} {'hint':<5} {'dialogs':<7} {'闲聊全过':<8} {'各检查通过'} {'QA中位字数'}")
    all_variants = {**VARIANTS, **DEEP_VARIANTS}
    for vname, vcfg in all_variants.items():
        passes, checks_total = 0, {}
        chat_n = 0
        for pname in CHAT_PROBES:
            r = by_tag.get(f"{vname}-{pname}")
            if not r:
                continue
            chat_n += 1
            reply = r["exchanges"][-1]["assistant"]
            checks = score_chat(reply)
            if all(checks.values()):
                passes += 1
            for k, ok in checks.items():
                checks_total[k] = checks_total.get(k, 0) + (1 if ok else 0)
        qa_lens = []
        for qname in QA_PROBES:
            r = by_tag.get(f"{vname}-{qname}")
            if r:
                qa_lens.append(len(r["exchanges"][-1]["assistant"]))
        qa_lens.sort()
        qa_med = qa_lens[len(qa_lens) // 2] if qa_lens else "-"
        detail = " ".join(f"{k}:{v}/{chat_n}" for k, v in checks_total.items())
        print(
            f"{vname:<4} {str(vcfg['hint']):<5} {str(vcfg['dialogs']):<7} "
            f"{passes}/{chat_n:<6} {detail}  {qa_med}"
        )


# ---------------------------------------------------------------- 冒烟

def smoke():
    RESULTS.mkdir(exist_ok=True)
    record = run_conversation(
        "smoke",
        hint=True,
        dialogs=True,
        turns=["今天有点无聊,随便聊聊?", "你平时喜欢干什么?"],
    )
    for i, ex in enumerate(record["exchanges"], 1):
        print(f"[回合{i}] {ex['assistant'][:160]}")
    sp = BASE / "homes" / "smoke" / "config" / "system-prompt.md"
    print(f"system-prompt.md 存在: {sp.exists()}")
    (RESULTS / "smoke.jsonl").write_text(
        json.dumps(record, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    print("冒烟通过")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "smoke"
    if cmd == "smoke":
        smoke()
    elif cmd == "run":
        run_matrix()
    elif cmd == "deep":
        run_matrix(deep=True)
    elif cmd == "score":
        score_matrix()
    elif cmd == "tools":
        run_tool_matrix()
    elif cmd == "tools-score":
        score_tool_matrix()
    else:
        print(f"未知子命令: {cmd}", file=sys.stderr)
        sys.exit(1)

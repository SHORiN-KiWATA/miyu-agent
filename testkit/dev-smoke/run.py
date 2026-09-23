#!/usr/bin/env python3
"""dev REPL daemon 形态冒烟(验收问题二复验)。

复验路径:隔离 home + 隔离 XDG_RUNTIME_DIR + 独立端口起 debug daemon →
PTY 进 `miyu dev` → 你好一轮 → 断言:无「找不到该会话」、会话挂 dev 人格、
/new 可用、重进复用指针不泄漏新会话、dev 记忆目录落盘。
真实供应商真实一轮(照 persona-ab 先例);绝不触碰线上 8300 daemon。
"""

import importlib.util
import json
import os
import shutil
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
MIYU = REPO / "target" / "debug" / "miyu"
BASE = Path(__file__).resolve().parent
HOME = BASE / "home"
RUN = BASE / "xdg-run"
LOGS = BASE / "results"
PORT = 18390

spec = importlib.util.spec_from_file_location(
    "persona_ab", REPO / "testkit" / "persona-ab" / "run.py"
)
persona_ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(persona_ab)


def build_home() -> None:
    if HOME.exists():
        shutil.rmtree(HOME)
    if RUN.exists():
        shutil.rmtree(RUN)
    (HOME / "config").mkdir(parents=True)
    RUN.mkdir(parents=True)
    cfg = persona_ab.load_real_config()
    for key in ("platforms", "voice", "alarm"):
        cfg.pop(key, None)
    cfg.setdefault("prompt", {})["active_persona"] = ""
    # 记忆开着:顺带验证 dev 独立记忆目录(data/personas/dev)落盘。
    cfg.setdefault("memory", {})["enabled"] = True
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def env() -> dict:
    e = dict(os.environ)
    e["MIYU_HOME"] = str(HOME)
    e["XDG_RUNTIME_DIR"] = str(RUN)
    e.pop("MIYU_DIRECT", None)
    e["TERM"] = "xterm-256color"
    e.setdefault("LANG", "zh_CN.UTF-8")
    return e


def daemon(*args: str) -> subprocess.CompletedProcess:
    # --port 只有 start/restart 认;stop 传了直接报错。
    port = ("--port", str(PORT)) if args[:1] != ("stop",) else ()
    return subprocess.run(
        [str(MIYU), "daemon", *port, *args],
        env=env(),
        capture_output=True,
        text=True,
        timeout=60,
    )


def db_query(sql: str, args=()):  # daemon 形态下 DB 仍在 home/state 下
    db = HOME / "state" / "conversation.db"
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
    try:
        return conn.execute(sql, args).fetchall()
    finally:
        conn.close()


class DevRepl(persona_ab.Repl):
    """persona-ab 的 PTY 驱动,换成 daemon 形态 `miyu dev`。"""

    def __init__(self, home: Path, log: Path):
        import fcntl
        import pty
        import struct
        import termios
        import threading

        self.home = home
        self.db = home / "state" / "conversation.db"
        log.parent.mkdir(parents=True, exist_ok=True)
        self.log = open(log, "wb")
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        self.proc = subprocess.Popen(
            [str(MIYU), "dev"],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env(),
            close_fds=True,
            preexec_fn=os.setsid,
        )
        os.close(slave)
        self._stop = False
        self.reader = threading.Thread(target=self._pump, daemon=True)
        self.reader.start()


def dev_sessions():
    return db_query(
        "SELECT session_id, name FROM sessions "
        "WHERE persona = 'dev' AND kind = 'user' ORDER BY created_at"
    )


def main() -> int:
    assert MIYU.exists(), f"先 cargo build:缺 {MIYU}"
    build_home()
    LOGS.mkdir(exist_ok=True)
    failures = []

    def check(name, ok, detail=""):
        print(("PASS " if ok else "FAIL ") + name + (f"  {detail}" if detail else ""))
        if not ok:
            failures.append(name)

    started = daemon("start")
    check("daemon start", started.returncode == 0, started.stderr.strip()[:200])
    try:
        # ---- 第一次进入 dev REPL:自举 + 一轮真实对话 ----
        repl = DevRepl(HOME, LOGS / "dev-first.log")
        time.sleep(3)
        reply = repl.chat("你好", timeout=180)
        check("首轮回复非空", bool(reply.strip()), reply[:60])
        sessions_after_first = dev_sessions()
        check(
            "会话挂 dev 人格",
            len(sessions_after_first) == 1,
            f"dev sessions={len(sessions_after_first)}",
        )
        raw = open(LOGS / "dev-first.log", "rb").read().decode("utf-8", "replace")
        check("无「找不到该会话」", "找不到该会话" not in raw)
        # ---- /new 在 dev REPL 内可用 ----
        repl.send("/new smoke-second")
        time.sleep(4)
        check("/new 增出第二个 dev 会话", len(dev_sessions()) == 2)
        repl.close()

        # ---- 第二次进入:复用指针,不泄漏新会话 ----
        repl2 = DevRepl(HOME, LOGS / "dev-second.log")
        time.sleep(6)
        repl2.close()
        check(
            "重进不泄漏新会话",
            len(dev_sessions()) == 2,
            f"dev sessions={len(dev_sessions())}",
        )

        # ---- dev 独立记忆目录 ----
        dev_mem = HOME / "data" / "personas" / "dev"
        check("dev 记忆目录落盘", dev_mem.exists(), str(dev_mem))
    finally:
        stopped = daemon("stop")
        check("daemon stop", stopped.returncode == 0, stopped.stderr.strip()[:120])

    print("=" * 40)
    print("全部通过" if not failures else f"失败 {len(failures)} 项: {failures}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""点开过的那一行，不该被自动收回去。

用户 09-19 两条：

1. 思考 tag 行已经点开了，**思考结束不该把它收起来**；
2. 时间线里已经有 tag 行被点开了，**这一段跑完不该收成一行 `› Worked for …`**
   ——该是展开着的 `⌄ Worked for …`，里面那一步也还开着。

两条是同一件事：「用户亲手点开的」比「默认怎么显示」优先。

跑法：

    cargo build
    python3 testkit/tui/keep_open.py

产物在 ~/.cache/miyu-keep-open/。

**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-keep-open"))
LINES = [f"第 {i} 行思考：这一行是为了把滚动窗喂满而写的占位内容。" for i in range(1, 25)]
REASONING = "\n".join(LINES)
SCROLL_WINDOW = 10


def rows_now(sink):
    return h.render(bytes(sink))


def find_row(rows, needle):
    for index, line in enumerate(rows):
        if (needle(line) if callable(needle) else needle in line):
            return index
    return None


def body_rows(rows):
    return sum(1 for line in rows if "行思考：" in line)


def shows_first_line(rows):
    """屏幕上看得到思考正文的**第一行**吗。

    这是「展开」与「滚动窗」的判据：正文长过滚动窗之后，窗里只有最后几行,
    只有展开态才从第 1 行开始。按行数判分不出来——两者行数可能一样多。
    """
    return any("第 1 行思考：" in line for line in rows)


def tap(master, head):
    """原地瞬时点一下：流式重画期间有间隔的点击会被当成拖动。"""
    os.write(master, f"\x1b[<0;7;{head + 1}M".encode())
    os.write(master, f"\x1b[<0;7;{head + 1}m".encode())


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    h.write_config()
    h.kill_stale_daemon()

    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(h.STUB_PORT),
            STUB_REASONING="1",
            STUB_REASONING_TEXT=REASONING,
            STUB_CHUNK_CHARS="6",
            STUB_CHUNK_SLEEP="0.05",
        ),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = tui = None
    report = {}
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        tui, master = h.spawn_tui()
        sink = bytearray()
        h.drain(master, 3.0, sink)

        os.write(master, "讲讲你的思路".encode())
        h.drain(master, 0.5, sink)
        os.write(master, b"\r")

        # 1. 等滚动思考窗，点开它
        deadline = time.time() + 25
        head = None
        while time.time() < deadline:
            h.drain(master, 0.4, sink)
            rows = rows_now(sink)
            head = find_row(rows, "思考中")
            # 等正文长过滚动窗:这样窗里就不会再有第 1 行,判据才立得住
            if head is not None and find_row(rows, "第 14 行思考") is not None:
                break
        if head is None:
            print("! 没等到滚动思考窗")
            return 2
        tap(master, head)
        h.drain(master, 1.0, sink)
        rows = rows_now(sink)
        (OUT / "01-expanded-while-thinking.txt").write_text("\n".join(rows), encoding="utf-8")
        report["点开之后展开了"] = shows_first_line(rows)

        # 2. 等这一段想完（抬头从「思考中」变成「已思考」）
        deadline = time.time() + 30
        while time.time() < deadline:
            h.drain(master, 0.2, sink)
            rows = rows_now(sink)
            if find_row(rows, "已思考") is not None or find_row(rows, h.is_fold_summary) is not None:
                break
        rows = rows_now(sink)
        (OUT / "02-thought-done.txt").write_text("\n".join(rows), encoding="utf-8")
        # 「想完不该被自动收起」这条判在收段之后那一格（`想完的那一步还开着`）：
        # 桩模型是「想完就说话」，中间那个窗口停不住，而落地之后那一步开不开
        # 正是同一个判据。

        # 3. 等这一轮说完话、整段收成 `Worked for …`
        deadline = time.time() + 40
        while time.time() < deadline:
            h.drain(master, 0.6, sink)
            rows = rows_now(sink)
            if find_row(rows, h.is_fold_summary) is not None:
                break
        h.drain(master, 2.0, sink)
        rows = rows_now(sink)
        (OUT / "03-after-fold.txt").write_text("\n".join(rows), encoding="utf-8")
        fold = find_row(rows, h.is_fold_summary)
        report["出现了收缩行"] = fold is not None
        if fold is not None:
            line = rows[fold]
            # `›` = 收着，`⌄` = 展开着
            report["收缩行是展开态"] = "⌄" in line and "›" not in line
            # 用户第一条：思考完成不该把已经点开的那一行收起来。落地之后
            # 那一步开不开就是它。
            report["想完的那一步还开着"] = shows_first_line(rows)
    finally:
        for process in (tui, daemon, stub):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    passed = failed = skipped = 0
    for name, ok in report.items():
        if ok is None:
            print(f"⏭️  {name}（这一轮没走到这个状态）")
            skipped += 1
            continue
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
        failed += not ok
    print(f"\n{passed}/{passed + failed} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())

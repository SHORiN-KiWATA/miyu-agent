#!/usr/bin/env python3
"""```mermaid 围栏在终端里要画成图，不是一坨源码。

在无头 cage + 真 kitty 里跑（复用 `testkit/kitty-image/run_headless.sh`）：
桩模型吐一段 mermaid，看屏幕上出没出图。判据是**截图里的近白像素**——
kitty 的图片是画到窗口上的，不在文本缓冲里，抓 pyte 是抓不到的；而 mermaid
的图有一整块白底画布，终端屏幕上则只有暗底加细笔画的文字，两者差两个数量级。

（先试过数"彩色像素"，不行：这张图是白底 + 灰蓝线条，饱和度比代码高亮还低。）

对照两组，缺一不可：

1. `mermaid` 围栏 → 出图（成片近白像素）；
2. `rust` 围栏 → 不出图，照常是带高亮的代码块（近白像素寥寥）。

两点要命的细节，都是第一版踩过的：

- miyu 的输出必须**直通 kitty 的 tty**。`capture_output=True` 会把图片协议的
  转义序列吞进管道，屏幕上什么都不会有，而 `is_native_kitty_terminal()` 看的是
  `TERM` 不是 isatty，所以它照样"成功"了——一片空屏配一条绿灯。
- `STUB_REPLY` 是桩模型**启动时**读的，不是每个请求读的。要换正文就得重起桩。

跑法：

    cargo build
    testkit/kitty-image/run_headless.sh python3 testkit/mermaid/terminal.py

产物在 ~/.cache/miyu-mermaid-term/（截图 + 像素数 + 报告）。
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-mermaid-term"))
HOME = Path("/tmp/miyu-mermaid-term/home")
REPO = Path(__file__).resolve().parents[2]
BIN = REPO / "target" / "debug" / "miyu"
SMOKE = REPO / "testkit" / "repl-smoke"

FLOW = (
    "flowchart TD\n"
    "    A[用户提问] --> B{要用工具吗}\n"
    "    B -->|要| C[调用工具]\n"
    "    B -->|不要| D[直接回答]\n"
    "    C --> D"
)
RUST = 'fn main() {\n    println!("hi");\n}'
CASES = (("mermaid", "mermaid", FLOW), ("rust", "rust", RUST))


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def write_config(port):
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(
            {
                "config_version": 3,
                "oobe_done": True,
                "active_provider": "stub",
                "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
                "providers": [
                    {
                        "id": "stub",
                        "display_name": "stub",
                        "base_url": f"http://127.0.0.1:{port}/v1",
                        "api_key": "sk-test",
                        "models": ["stub-model"],
                        "default_model": "stub-model",
                    }
                ],
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )


def start_stub(port, reply):
    """每个用例起一只自己的桩——正文是启动时读的。"""
    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(port), STUB_REPLY=reply),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(80):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return stub
        except OSError:
            time.sleep(0.1)
    stub.kill()
    raise RuntimeError(f"桩模型没起来：{port}")


def grim(path):
    """截无头输出。grim 不在就返回 None，调用方按「没法判」处理。"""
    if not shutil.which("grim"):
        return None
    subprocess.run(["grim", str(path)], capture_output=True, timeout=30)
    return path if path.exists() else None


# 一整块白底画布 vs 一屏文字：阈值放在两个数量级中间，怎么都不会擦边。
WHITE_FLOOR = 180
WHITE_PIXELS = 20000


def white_pixels(path):
    """截图里有多少近白像素——mermaid 那张图的画布底色。"""
    try:
        from PIL import Image
    except ImportError:
        return None
    image = Image.open(path).convert("RGB")
    return sum(1 for r, g, b in image.getdata() if min(r, g, b) >= WHITE_FLOOR)


def run_case(tag, lang, body, port, report):
    reply = f"看这张图：\n\n```{lang}\n{body}\n```\n"
    stub = start_stub(port, reply)
    try:
        # 清屏：上一轮的画面还在，不清两张截图会互相污染。
        sys.stdout.write("\033[H\033[2J\033[3J")
        sys.stdout.flush()
        with (OUT / f"{tag}.log").open("wb") as err:
            subprocess.run(
                [str(BIN), "画个图"],
                env=dict(os.environ, MIYU_HOME=str(HOME), MIYU_DIRECT="1", MIYU_TUI="0"),
                stdout=None,  # 直通 kitty 的 tty，图片才画得出来
                stderr=err,
                stdin=subprocess.DEVNULL,
                timeout=180,
            )
        time.sleep(2.0)
    finally:
        stub.terminate()
        try:
            stub.wait(timeout=5)
        except subprocess.TimeoutExpired:
            stub.kill()

    shot = grim(OUT / f"{tag}.png")
    report[f"{tag}: 截到图"] = shot is not None
    if not shot:
        return None
    pixels = white_pixels(shot)
    report[f"{tag}: 数得出近白像素"] = pixels is not None
    return pixels


def main():
    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    if "kitty" not in os.environ.get("TERM", "") and not os.environ.get("KITTY_WINDOW_ID"):
        print(
            "! 要在真 kitty 里跑："
            "testkit/kitty-image/run_headless.sh python3 testkit/mermaid/terminal.py",
            file=sys.stderr,
        )
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    OUT.mkdir(parents=True, exist_ok=True)

    port = free_port()
    write_config(port)
    report, counts = {}, {}
    for tag, lang, body in CASES:
        counts[tag] = run_case(tag, lang, body, port, report)

    if counts.get("mermaid") is not None:
        report["mermaid 围栏画出了图"] = counts["mermaid"] > WHITE_PIXELS
    if counts.get("rust") is not None:
        report["rust 围栏没被当成图"] = counts["rust"] < WHITE_PIXELS
    if counts.get("mermaid") is not None and counts.get("rust") is not None:
        # 差距本身也是判据：出图那次该比不出图那次多一个数量级。
        report["出图那次明显比代码块那次白"] = counts["mermaid"] > counts["rust"] * 10 + 1000

    (OUT / "pixels.json").write_text(json.dumps(counts), encoding="utf-8")
    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    passed = sum(1 for ok in report.values() if ok)
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{passed}/{len(report)} passed  近白像素 {counts}")
    return 0 if report and passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())

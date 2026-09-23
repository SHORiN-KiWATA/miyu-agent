#!/usr/bin/env python3
"""全屏 TUI 的**真机**验收：无头 kitty 里跑一遍，grim 截真像素。

pyte 只还原字符网格，图形协议那一段它看不见——「图有没有真的画出来」和「公式
渲染成了什么」只能在真 kitty 里看。这个脚本在 cage 的无头输出里起一个真 kitty
（不碰用户桌面），在里面跑全屏 TUI，截三张图：

    tui-image.png          图片刚打出来
    tui-image-repaint.png  敲一个字触发重画之后（图还在 = 占位格真的进了缓冲）
    tui-table-math.png     markdown 表格 + LaTeX 行间公式

跑法：

    cargo build --release
    testkit/kitty-image/run_headless.sh python3 testkit/tui/kitty_shot.py

产物在 $OUT（默认 ~/.cache/miyu-tui-kitty）。
"""

import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
KITTY = REPO / "testkit" / "kitty-image"
sys.path.insert(0, str(KITTY))
import ghost_probe as probe  # noqa: E402

OUT = Path(os.environ.get("OUT") or "~/.cache/miyu-tui-kitty").expanduser()
# 绝对路径：子进程是拿 `cwd=OUT` 起的，相对路径会按 OUT 解析然后找不到，
# 抛出来的异常还打在 kitty 窗口里、外面看不见。
BIN = Path(os.environ.get("BIN") or (REPO / "target" / "release" / "miyu")).resolve()
HOME = OUT / "home"
RUN_DIR = OUT / "run"
STUB_PORT = int(os.environ.get("STUB_PORT", "18497"))
PORT = int(os.environ.get("MIYU_TUI_PORT", "18437"))
STUB_LOG = OUT / "stub.jsonl"
IMAGE = OUT / "sample.png"
IMAGE_COLS, IMAGE_ROWS = 28, 8

PROMPT_IMAGE = "STUB_SCRIPT 把那张图显示出来"
PROMPT_TEXT = "STUB_SCRIPT 再来一段"

# 表格 + 行间公式：两样都按**正文区**排版，越界就会在截图里看出来。
SAMPLE_TEXT = """这是一段用来看版式的正文。

| 终端 | 图形协议 | 备注 |
|---|---|---|
| kitty | 自家协议 | 占位符走 Unicode |
| WezTerm | 兼容 kitty | 也支持 iTerm2 |
| foot | sixel | Wayland 原生 |

行间公式：

$$
E = mc^2 \\qquad \\int_{0}^{\\infty} e^{-x^2}\\,dx = \\frac{\\sqrt{\\pi}}{2}
$$

结束。
"""
# 想只看"工具在前、图在后"的顺序时，用一段短正文跑，免得时间线被顶出屏幕。
SAMPLE_TEXT = os.environ.get("SHOT_TEXT") or SAMPLE_TEXT


def make_image(cell_w, cell_h):
    """一张一眼认得出的图：蓝底 + 白色对角线 + 四角红点。

    纯色块在截图里和「一片空白」太像；有花纹才分得清"图真的画出来了"和
    "占位格铺了一片但没有图"。
    """
    from PIL import Image, ImageDraw

    width, height = IMAGE_COLS * cell_w, IMAGE_ROWS * cell_h
    image = Image.new("RGB", (width, height), (30, 80, 200))
    draw = ImageDraw.Draw(image)
    draw.line([(0, 0), (width, height)], fill=(255, 255, 255), width=3)
    draw.line([(0, height), (width, 0)], fill=(255, 255, 0), width=3)
    for x, y in ((4, 4), (width - 12, 4), (4, height - 12), (width - 12, height - 12)):
        draw.rectangle([x, y, x + 8, y + 8], fill=(230, 40, 40))
    image.save(IMAGE)


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{
            "id": "stub",
            "display_name": "Stub",
            "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
            "protocol": "openai-chat",
            "api_key": "stub",
            "models": ["stub-model"],
        }],
        "memory": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.2)
    return False


def env_for():
    env = dict(os.environ)
    for key in ("XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"):
        env.pop(key, None)
    env["MIYU_HOME"] = str(HOME)
    env["XDG_RUNTIME_DIR"] = str(RUN_DIR)
    env["MIYU_TUI"] = "1"
    return env


def screen_text():
    return probe.kitten("get-text", "--extent", "screen").stdout


def wait_for(marker, timeout=60.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if marker in screen_text():
            return True
        time.sleep(0.25)
    return False


def wait_quiet(seconds=2.0, timeout=60.0):
    last, since, start = None, time.time(), time.time()
    while time.time() - start < timeout:
        text = screen_text()
        if text != last:
            last, since = text, time.time()
        elif time.time() - since > seconds:
            return text
        time.sleep(0.25)
    return last or ""


def log(message):
    """日志写文件：脚本是在 kitty 窗口里跑的，print 出来的东西外面看不见。"""
    OUT.mkdir(parents=True, exist_ok=True)
    with (OUT / "shot.log").open("a", encoding="utf-8") as handle:
        handle.write(f"{time.strftime('%H:%M:%S')} {message}\n")


def main():
    if not BIN.exists():
        print(f"! 先 cargo build --release：{BIN} 不存在", file=sys.stderr)
        return 2
    OUT.mkdir(parents=True, exist_ok=True)
    if (OUT / "shot.log").exists():
        (OUT / "shot.log").unlink()
    for path in (HOME, RUN_DIR):
        if path.exists():
            shutil.rmtree(path)
    RUN_DIR.mkdir(parents=True)
    if STUB_LOG.exists():
        STUB_LOG.unlink()

    rows, cols, xpixel, ypixel = probe.term_geometry()
    cell_w, cell_h = max(xpixel // cols, 1), max(ypixel // rows, 1)
    log(f"终端 {cols}x{rows}，格子 {cell_w}x{cell_h}  bin={BIN}")
    write_config()
    make_image(cell_w, cell_h)

    stub = subprocess.Popen(
        [sys.executable, str(KITTY / "stub_llm.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(STUB_PORT),
            STUB_LOG=str(STUB_LOG),
            STUB_IMAGE=str(IMAGE),
            STUB_IMAGE_SIZE=f"{IMAGE_COLS}x{IMAGE_ROWS}",
            STUB_TEXT=SAMPLE_TEXT,
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    daemon = None
    miyu = None
    try:
        if not wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"):
            log("! 桩模型没起来")
            return 2
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(PORT)],
            env=env_for(), cwd=str(OUT),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not wait_http(f"http://127.0.0.1:{PORT}/api/config", timeout=40):
            log("! daemon 没起来")
            return 2

        miyu = subprocess.Popen([str(BIN)], env=env_for(), cwd=str(OUT))
        wait_quiet(1.5, timeout=40)
        log("首屏")
        probe.screenshot("tui-start")

        probe.kitten("send-text", PROMPT_IMAGE)
        time.sleep(0.3)
        probe.kitten("send-key", "enter")
        wait_quiet(2.5, timeout=120)
        log("图片那一轮跑完")
        probe.screenshot("tui-image")

        # 敲一个字逼它重画一帧：图片的占位格如果真的进了缓冲，重画之后图还在。
        probe.kitten("send-text", "x")
        time.sleep(1.2)
        probe.screenshot("tui-image-repaint")
        probe.kitten("send-key", "backspace")
        time.sleep(0.4)

        probe.kitten("send-text", PROMPT_TEXT)
        time.sleep(0.3)
        probe.kitten("send-key", "enter")
        wait_quiet(2.5, timeout=120)
        log("第二轮跑完")
        probe.screenshot("tui-table-math")

        # SHOT_REPAINT_LAST=1：第二轮也来一次「敲个字逼它重画」。正文里带图时
        # （比如 ```mermaid）要验的是同一件事——占位格进了缓冲，重画之后图还在。
        if os.environ.get("SHOT_REPAINT_LAST"):
            probe.kitten("send-text", "x")
            time.sleep(1.2)
            probe.screenshot("tui-table-math-repaint")
            probe.kitten("send-key", "backspace")
            time.sleep(0.4)
        (OUT / "screen.txt").write_text(screen_text(), encoding="utf-8")

        # SHOT_CLICK_TEXT=<一段文字>：在屏幕上找到它，往那儿真点一下。
        #
        # 全屏把鼠标捕获走了，"点链接"是 Miyu 自己接的（`select::url_at` +
        # `open_url`），所以「点了有没有反应」只有真点一次才验得到。无头 cage 里
        # 没有指针设备，于是直接把 **SGR 鼠标序列**当输入喂进去——crossterm 认的
        # 就是这个，和真鼠标走的是同一条路。
        click_text = os.environ.get("SHOT_CLICK_TEXT")
        if click_text:
            lines = screen_text().splitlines()
            hit = next(
                ((y, line.index(click_text)) for y, line in enumerate(lines) if click_text in line),
                None,
            )
            if hit is None:
                log(f"! 屏幕上找不到 {click_text!r}，没法点")
            else:
                # SGR：行列都是 1 起算；`M` 是按下、`m` 是松开。列取标签中间，
                # 免得压在边界上。
                row, col = hit[0] + 1, hit[1] + 2
                probe.kitten("send-text", f"\x1b[<0;{col};{row}M")
                time.sleep(0.15)
                probe.kitten("send-text", f"\x1b[<0;{col};{row}m")
                time.sleep(1.5)
                log(f"点了 ({row},{col}) 上的 {click_text!r}")
                probe.screenshot("tui-after-click")
        log(f"产物：{OUT}")
        names = ["tui-start", "tui-image", "tui-image-repaint", "tui-table-math"]
        if os.environ.get("SHOT_REPAINT_LAST"):
            names.append("tui-table-math-repaint")
        for name in names:
            path = OUT / f"{name}.png"
            log(f"  {path}  {'有' if path.exists() else '缺'}")
        return 0
    finally:
        for process in (miyu, daemon, stub):
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()


if __name__ == "__main__":
    raise SystemExit(main())

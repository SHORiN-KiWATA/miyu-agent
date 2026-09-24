#!/usr/bin/env python3
"""长文转图的换栏与尺寸（用户 09-24）：`<br>` 换行、换栏不留大片空档、长宽跟着内容走。

    BIN=<miyu> python3 testkit/qq-longtext/columns.py

直接驱动真二进制的渲染子进程（`miyu __renderer-worker` + `MIYU_INTERNAL_RENDERER_WORKER=1`，
长度前缀的 JSON 请求，二进制帧回 PNG），喂一篇和用户截图同结构的合成长文（标题、说明、
格子里带 `<br>` 的大表、小节标题），数像素判定：

  renders             渲出一张图
  square_ish          成图接近方形（宽高比 0.75–1.5）；以前是 6 栏 6048×2600 的扁长条
  height_past_old_cap 高度超过原来默认的上限 2600
  no_big_gaps         除最后一栏外，每栏的内容底边都到最高那栏的 80% 以上
  br_breaks_lines     一格里 8 个 `<br>` 分出的 8 行真的换了行（图明显变高）
  max_height_ignored  请求里旧的 max_height 不再起作用（填 1000 和填 2600 渲出来一样大）

不需要 daemon、不碰 8300；家目录是临时目录，跑完删掉。
"""
import io
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image

BIN = Path(os.environ["BIN"])
PADDING, COLUMN_WIDTH, COLUMN_GAP = 64, 960, 32
# 旧版渲染子进程缺 max_height 就拒收请求；旧 daemon 发的是用户配置里的值（默认 2600）。
# 带上它，旧构建也能跑出图来对照；新版不读它。
BASE_CONFIG = {"theme": "paper", "max_height": 2600, "font_size": 36, "code_font_size": 30,
               "padding": PADDING, "font": "", "title_font": "", "code_font": "", "emoji_font": ""}
results = []


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + detail) if detail else ''}")


def read_exact(stream, size):
    data = stream.read(size)
    if len(data) != size:
        raise RuntimeError(f"worker closed early ({len(data)}/{size} bytes)")
    return data


def read_u32(stream):
    return struct.unpack(">I", read_exact(stream, 4))[0]


def render(markdown, extra_config=None):
    """起一个渲染子进程渲一张图，返回 PIL 图像。"""
    home = tempfile.mkdtemp(prefix="miyu-longtext-columns-")
    config = dict(BASE_CONFIG, **(extra_config or {}))
    payload = json.dumps({"markdown": markdown, "config": config}).encode()
    env = dict(os.environ, MIYU_HOME=home, MIYU_INTERNAL_RENDERER_WORKER="1")
    worker = subprocess.Popen([str(BIN), "__renderer-worker"], env=env, cwd=home,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        worker.stdin.write(struct.pack(">I", len(payload)) + payload)
        worker.stdin.flush()
        if read_exact(worker.stdout, 1)[0] != 0:
            message = read_exact(worker.stdout, read_u32(worker.stdout)).decode(errors="replace")
            raise RuntimeError(f"render failed: {message}")
        if read_u32(worker.stdout) < 1:
            raise RuntimeError("worker returned no image")
        read_u32(worker.stdout)  # width
        read_u32(worker.stdout)  # height
        read_exact(worker.stdout, read_u32(worker.stdout))  # mime
        png = read_exact(worker.stdout, read_u32(worker.stdout))
    finally:
        worker.stdin.close()
        worker.kill()
        worker.wait(timeout=10)
        shutil.rmtree(home, ignore_errors=True)
    return Image.open(io.BytesIO(png)).convert("RGB")


def long_document():
    """和用户截图同结构：标题 + 说明 + 几张格子里带 <br> 的大表 + 小节标题 + 列表。"""
    tiers = ["S 级", "A 级", "B 级", "C 级"]
    parts = ["# 某游戏职业与种族深度选择攻略\n\n",
             "> **数据口径**：基于封闭测试服实测与测试天梯数据，涵盖首发地下城、团队副本、战场与野外对抗。\n\n"]
    # 两节：再长旧版渲染器就超单页像素上限、整张渲不出来，没法逐项对照。
    for section in range(1, 3):
        parts.append(f"## {'一二'[section - 1]}、第 {section} 部分梯队榜\n\n")
        parts.append("> 说明：坦克与纯治疗不计入输出排行，这一栏只比较输出专精的综合表现。\n\n")
        parts.append("| 梯队 | 包含专精 | 综合定位与实战表现 |\n| :--- | :--- | :--- |\n")
        for row in range(10):
            tier = tiers[row % 4]
            detail = "<br>".join(f"• 专精{row}-{item}：输出稳定，单体与多目标兼顾，机动性与自保能力俱佳。"
                                  for item in range(3))
            parts.append(f"| {tier}<br>(第 {row} 档) | 专精甲、专精乙<br>专精丙 | {detail} |\n")
        parts.append("\n### 小结\n\n")
        parts.append("这一部分的要点是先看版本答案，再按自己的操作习惯挑，别一味追强度。\n\n")
    parts.append("## 三、首发终极推荐速查\n\n")
    for item in range(6):
        parts.append(f"- 推荐 {item}：首推专精甲（后期真神），备选专精乙，团队副本与野外都好用。\n")
    return "".join(parts)


def content_bottoms(image, columns):
    """每一栏最后一行有内容的 y（与左上角留白颜色不同即算内容）。"""
    background = image.getpixel((PADDING // 2, PADDING // 2))
    pixels = image.load()
    bottoms = []
    for column in range(columns):
        left = PADDING + column * (COLUMN_WIDTH + COLUMN_GAP)
        bottom = 0
        for y in range(image.height - 1, -1, -1):
            if any(pixels[x, y] != background for x in range(left, left + COLUMN_WIDTH, 3)):
                bottom = y
                break
        bottoms.append(bottom)
    return bottoms


def main():
    document = long_document()
    try:
        image = render(document)
    except Exception as error:  # noqa: BLE001 — 渲染失败也要报成一项判定
        check(False, "renders", str(error))
        print(f"{sum(results)}/{len(results)} passed")
        sys.exit(1)
    check(True, "renders", f"{image.width}x{image.height}")
    aspect = image.width / image.height
    check(0.75 <= aspect <= 1.5, "square_ish", f"{image.width}x{image.height}, {aspect:.2f}")
    check(image.height > 2600, "height_past_old_cap", f"height {image.height}")

    columns = round((image.width - 2 * PADDING + COLUMN_GAP) / (COLUMN_WIDTH + COLUMN_GAP))
    bottoms = content_bottoms(image, columns)
    tallest = max(bottoms)
    short = [index for index, bottom in enumerate(bottoms[:-1]) if bottom < tallest * 0.8]
    check(columns >= 2 and not short, "no_big_gaps", f"{columns} columns, bottoms {bottoms}")

    cell = "<br>".join(f"第 {line} 行" for line in range(1, 9))
    tall = render(f"| 分行 |\n| --- |\n| {cell} |\n")
    check(tall.height > 500, "br_breaks_lines", f"height {tall.height}")

    capped = render(document, {"max_height": 1000})
    check(capped.size == image.size, "max_height_ignored", f"{capped.size} vs {image.size}")

    print(f"{sum(results)}/{len(results)} passed")
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()

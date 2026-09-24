#!/usr/bin/env python3
"""QQ 长文转图：地址要有链接色（标题不染），单个换行要照画成换行。

09-23 用户反馈「长文转图的链接颜色又没了」。提示词要她给来源写 `title (url)`
（09-22 起括号内带空格），这是纯文本；图渲染器只认 Markdown 链接语法，于是整行
和正文同色。同一张图里「每个来源一行」也被 CommonMark 并成了一段。

直接驱动真二进制的渲染子进程（daemon 平时就是这么起它的：`miyu __renderer-worker`
+ `MIYU_INTERNAL_RENDERER_WORKER=1`，长度前缀的 JSON 请求，二进制帧回 PNG），
不起 daemon、不连 QQ、不花模型额度。数成图里链接色的像素。

Run: python3 testkit/qq-longtext/link_color.py --binary /absolute/path/to/miyu
产物：$OUT（默认 ~/.cache/miyu-qq-longtext-link）/*.png
"""

import argparse
import io
import json
import os
import struct
import subprocess
from pathlib import Path

from PIL import Image
import sys
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

# 与 crates/miyu-hosts/src/platforms/plugins/renderer/paint.rs 的调色盘一致。
LINK = {"paper": (45, 95, 125), "light": (48, 101, 190), "dark": (104, 179, 255)}
SOURCES = (
    "模型 & 价格 | DeepSeek API Docs (https://api-docs.deepseek.com/zh-cn/quick_start/pricing/)\n"
    "Silverhairfx/DictaPulse ( https://github.com/Silverhairfx/DictaPulse )\n"
)

results = []


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + detail) if detail and not ok else ''}")


def read_exact(stream, size):
    data = stream.read(size)
    if len(data) != size:
        raise RuntimeError(f"worker closed early ({len(data)}/{size} bytes)")
    return data


def read_u32(stream):
    return struct.unpack(">I", read_exact(stream, 4))[0]


def render(binary, home, markdown, theme):
    """起一个渲染子进程渲一张图，返回 PIL 图像。"""
    config = {"theme": theme, "max_height": 2600, "font_size": 36, "code_font_size": 30,
              "padding": 64, "font": "", "title_font": "", "code_font": "", "emoji_font": ""}
    payload = json.dumps({"markdown": markdown, "config": config}).encode()
    env = dict(os.environ, MIYU_HOME=str(home), MIYU_INTERNAL_RENDERER_WORKER="1")
    worker = subprocess.Popen([str(binary), "__renderer-worker"], env=env, cwd=str(home),
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL)
    try:
        worker.stdin.write(struct.pack(">I", len(payload)) + payload)
        worker.stdin.flush()
        status = read_exact(worker.stdout, 1)[0]
        if status != 0:
            message = read_exact(worker.stdout, read_u32(worker.stdout)).decode(errors="replace")
            raise RuntimeError(f"render failed: {message}")
        count = read_u32(worker.stdout)
        if count < 1:
            raise RuntimeError("worker returned no image")
        read_u32(worker.stdout)  # width
        read_u32(worker.stdout)  # height
        read_exact(worker.stdout, read_u32(worker.stdout))  # mime
        png = read_exact(worker.stdout, read_u32(worker.stdout))
    finally:
        worker.stdin.close()
        worker.kill()
        worker.wait(timeout=10)
    return Image.open(io.BytesIO(png)).convert("RGB")


def link_pixels(image, theme):
    target = LINK[theme]
    return sum(1 for pixel in image.get_flattened_data() if pixel == target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    out = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-qq-longtext-link"))
    out.mkdir(parents=True, exist_ok=True)
    home = sandbox_dir.make("miyu-longtext-link-")

    for theme in ("paper", "light", "dark"):
        image = render(binary, home, SOURCES, theme)
        image.save(out / f"sources-{theme}.png")
        count = link_pixels(image, theme)
        check(count > 200, f"{theme}: 纯文本来源行是链接色", f"{count} 个链接色像素")

    # 只有地址上色，标题不染（用户 09-23）。标题占住左边一段、整行不折（折了地址
    # 会落到第二行最左边，裁进来的就是地址了），裁出标题那段数。
    title = "这是一个较长的来源标题"
    for name, markdown in (("纯文本", f"{title} (https://a.example)\n"),
                           ("Markdown", f"[{title}](https://a.example)\n")):
        image = render(binary, home, markdown, "paper")
        image.save(out / f"title-{name}.png")
        left = image.crop((0, 0, 440, image.height))
        check(link_pixels(left, "paper") == 0 and link_pixels(image, "paper") > 100,
              f"{name}「标题 (地址)」只有地址是链接色，标题不染",
              f"左半边 {link_pixels(left, 'paper')} / 全图 {link_pixels(image, 'paper')}")

    prose = render(binary, home, "只是一段普通的正文,没有地址,也没有链接。\n", "paper")
    check(link_pixels(prose, "paper") == 0, "没有地址的正文不带链接色")

    bare = render(binary, home, "价格表见 https://api-docs.deepseek.com/zh-cn/quick_start/pricing/。\n",
                  "paper")
    bare.save(out / "bare-url.png")
    check(link_pixels(bare, "paper") > 200, "句中裸地址是链接色")

    code = render(binary, home, "代码里的 `https://example.com` 不算链接。\n", "paper")
    check(link_pixels(code, "paper") == 0, "行内代码里的地址不上链接色")

    # 八条来源分行写 vs 并成一行写：分行的那张要更高（单个换行照画成换行）。
    # 条数要多到撑过页面的最小高度（360px），两三行的话两张图一样高。
    entries = [f"{name} (https://{name.lower()}.example)" for name in "ABCDEFGH"]
    split = render(binary, home, "\n".join(entries) + "\n", "paper")
    joined = render(binary, home, " ".join(entries) + "\n", "paper")
    split.save(out / "split-lines.png")
    check(split.height > joined.height, "单个换行照画成换行（分行写的图更高）",
          f"分行 {split.height}px / 并行 {joined.height}px")

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({out})")
    raise SystemExit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""长图里的 mermaid 图（用户 09-24）：字够大、一栏装得下、铺在纸面上。

    BIN=<miyu> python3 testkit/qq-longtext/mermaid.py

驱动方式同 `columns.py`（真二进制的渲染子进程）。导图取自用户截图那篇（四层）。
每次只渲一张图，量它在页面上占的范围：

  mindmap_tree_fits  导图收进一栏（宽 ≤ 960）且够高（≥ 600）：树状排、字放大后的样子；
                     以前放射状铺开再整张缩，只有 330 左右高、字 10px
  mindmap_on_paper   导图底下不是一整块白板（纯白像素 < 3%）：底色换成了纸面色
  small_not_blown_up 只有一个节点的小图按字号定大小（宽 300–600），不再铺满一栏；以前小图
                     一律撑到栏宽，字比正文还大（截图里 S/A 那张）
"""
import importlib.util
import sys
from pathlib import Path

spec = importlib.util.spec_from_file_location("columns", Path(__file__).with_name("columns.py"))
columns = importlib.util.module_from_spec(spec)
spec.loader.exec_module(columns)
check, render, results = columns.check, columns.render, columns.results

MINDMAP = """```mermaid
mindmap
  root((魔兽世界：无限))
    中立新种族
      天裔 Skyborne
        联盟高阶阵营: 法力回涌/法师解禁
        部落御风者: 移速增益/萨满解禁
        共通特质: 凌空滑翔/急速提升
    全新职业生态
      混合职业救赎
        防骑原生嘲讽与群拉
        恢复萨实装激流与图腾优化
        暗牧/鸟德回蓝续航质变
      经典重制
        狂暴战怒气循环优化
        痛苦术Dot解除限制
        敏锐贼暗影步控制天花板
    环境划分
      PVE 团本与5人本: 重视减伤链与辅助协同
      PVP 野外与战场: 节奏更快、重视强解与爆发
```
"""


def diagram_box(image):
    """页面上和纸面颜色不同的那一块（这页只有一张图）。"""
    background = image.getpixel((8, 8))
    pixels = image.load()
    xs, ys = [], []
    for y in range(0, image.height, 2):
        for x in range(0, image.width, 2):
            if pixels[x, y] != background:
                xs.append(x)
                ys.append(y)
    if not xs:
        return None
    return min(xs), min(ys), max(xs), max(ys)


def white_share(image, box):
    left, top, right, bottom = box
    pixels = image.load()
    total = white = 0
    for y in range(top, bottom + 1, 2):
        for x in range(left, right + 1, 2):
            total += 1
            white += pixels[x, y] == (255, 255, 255)
    return white / max(total, 1)


def main():
    mindmap = render(MINDMAP)
    box = diagram_box(mindmap)
    width, height = (box[2] - box[0], box[3] - box[1]) if box else (0, 0)
    check(box and width <= 960 and height >= 600, "mindmap_tree_fits", f"diagram {width}x{height}")
    share = white_share(mindmap, box) if box else 1.0
    check(share < 0.03, "mindmap_on_paper", f"white {share:.1%}")

    single = render('```mermaid\ngraph TD\n    A["敏锐贼: 影遁/偷袭/暗影步/双伺机待发"]\n```\n')
    box = diagram_box(single)
    width = box[2] - box[0] if box else 0
    check(300 <= width <= 600, "small_not_blown_up", f"diagram width {width}")

    print(f"{sum(results)}/{len(results)} passed")
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()

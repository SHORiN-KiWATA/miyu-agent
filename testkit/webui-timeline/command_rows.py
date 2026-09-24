#!/usr/bin/env python3
"""「运行命令」那一步的新版式：抬头给 short_title，命令本身排在底下。

    BIN=<miyu 二进制> [WEB=<web 目录>] python3 testkit/webui-timeline/command_rows.py

形状（和 TUI 对齐）：

    <图标> 运行命令 <读秒> <short_title>
           <命令的第 1 行>
           <命令的第 2 行>
           ⋮                       ← 装不下时

要点都是有来由的，别随手改：

- **抬头右边是 `title` 不是命令文本**。Rust 侧 `tool_display::command_peek()` 取的
  就是模型自报的 `title`，注释写着「命令就印在抬头底下」；WebUI 以前拿命令当窥视，
  两边不一致。
- **读秒定格成真实耗时**。ticker 算的是「卡片建出来到现在」，一条 23ms 就跑完的
  命令按那个数显示毫无意义。
- **`display.command_output_lines` 数的是「屏幕上占几行」**，不是「命令有几行」。
  一条很长的单行命令要软换行把这几行空间用满（用户 09-20），不是截成一行加省略号。
- **`⋮` 挂在裁剪容器外面**，否则它自己也会被 max-height 裁掉。
- **展开之后要看得出哪块是命令、哪块是输出**：标签（参数／结果）显示出来，块之间
  一道细线（用户 09-20：「不知道输出和命令的分界线在哪」）。
- **命令输出**不在收起态露（TUI 同规矩：输出点开才看）。

产物：$OUT/{command-rows.json, command-step-*.png, command-expanded.png}
"""

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import run as harness  # noqa: E402

from playwright.sync_api import sync_playwright  # noqa: E402

# 12 行短命令：比默认的 8 行多，把「裁剪 + ⋮」逼出来。
COMMAND = "\n".join(
    [
        "set -euo pipefail",
        "cd /home/shorin/Documents/github/Miyu",
        "echo '== 第 3 行 =='",
        "ls -la | head -5",
        "echo '== 第 5 行 =='",
        "grep -rn 'mermaid' web/app.js | head -3",
        "echo '== 第 7 行 =='",
        "date +%s",
        "echo '== 第 9 行 =='",
        "uname -a",
        "echo '== 第 11 行 =='",
        "echo 收尾",
    ]
)
# 一条很长的**单行**命令：要软换行把空间用满，不是一行加省略号。
LONG = (
    "echo '———— [1/8] 内核与运行时长 ————'; uname -srm; uptime -p; "
    "echo '———— [2/8] 内存与交换 ————'; free -h; "
    "echo '———— [3/8] 磁盘 ————'; lsblk -o NAME,SIZE,TYPE,MOUNTPOINT | head -12; "
    "echo '———— [4/8] 负载 ————'; cat /proc/loadavg; "
    "echo '———— [5/8] 包数 ————'; pacman -Q | wc -l"
)
TITLE = "看看仓库里有什么"

PROBE = """() => {
  const card = document.querySelector('.proc-steps .tool-card.is-command');
  if (!card) return { missing: true };
  const title = card.querySelector('.tool-title');
  const txt = (el) => (el ? (el.textContent || '').trim() : null);
  const preview = card.querySelector('.tool-command-preview');
  const more = card.querySelector('.tool-command-more');
  const rowEls = [...card.querySelectorAll('.tool-command-line')];
  const box = preview ? preview.getBoundingClientRect() : null;
  const lineHeight = preview ? parseFloat(getComputedStyle(preview).lineHeight) : 0;
  const outputPreview = card.querySelector('.tool-command-output-preview');
  return {
    titleParts: [...title.children].map((el) => ({ cls: el.className, text: txt(el) })),
    seconds: txt(card.querySelector('.tool-command-seconds')),
    summary: txt(card.querySelector('.tool-summary')),
    rows: rowEls.map((r) => r.textContent),
    // 每条逻辑行在屏幕上占了几行（软换行之后 > 1）
    rowSpans: rowEls.map((r) =>
      Math.round(r.getBoundingClientRect().height / (lineHeight || 1))
    ),
    lineHeight,
    previewVisible: !!(box && box.width > 0 && box.height > 0),
    // 容器卡到了几行。**读自定义属性，别拿 getBoundingClientRect 去除行高**：
    // `.app-shell` 上有 `zoom: 1.1`，量出来的是缩放后的像素，除以未缩放的
    // line-height 会把 7 行算成 8（这个错差点让一条断言蒙混过关）。
    capRows: preview
      ? Number(getComputedStyle(preview).getPropertyValue('--command-rows')) || 0
      : 0,
    previewLeft: box ? Math.round(box.left) : 0,
    iconRight: Math.round(card.querySelector('.tool-icon').getBoundingClientRect().right),
    moreVisible: !!(more && more.getBoundingClientRect().height > 0),
    moreLeft: more ? Math.round(more.getBoundingClientRect().left) : 0,
    outputPreviewVisible: !!(
      outputPreview && outputPreview.getBoundingClientRect().height > 0
    ),
    // 末尾那一步的命令行旁边也得有线：连线只画到最后一个节点圆心的话，那儿是空的
    // （后面再跟个工具才"看起来有线"，那根其实是下一段的连线）。
    rail: (() => {
      const line = card.closest('.proc-line');
      const rail = line && line.querySelector('.proc-rail');
      const cards = line ? [...line.querySelectorAll('.tool-card.is-command')] : [];
      const lastRows = cards.length
        ? cards[cards.length - 1].querySelector('.tool-command-preview')
        : null;
      if (!rail || !lastRows) return null;
      const r = rail.getBoundingClientRect();
      const w = lastRows.getBoundingClientRect();
      const steps = line.querySelector('.proc-steps');
      const kids = steps ? [...steps.children] : [];
      const lastStep = kids.reverse().find((st) => {
        const n = st.querySelector(':scope > .tool-head > .tool-icon,'
          + ' :scope > summary > .reasoning-icon');
        return n && n.offsetParent;
      });
      return {
        railBottom: Math.round(r.bottom),
        railTop: Math.round(r.top),
        lastRowsBottom: Math.round(w.bottom),
        lastRowsTop: Math.round(w.top),
        stepCount: kids.length,
        lastStepClass: lastStep ? lastStep.className : null,
        trailingInLastStep: lastStep
          ? lastStep.querySelectorAll(':scope > .tool-command-preview').length
          : -1,
        lineOpen: line.classList.contains('is-open'),
      };
    })(),
  };
}"""

# 思考滚动窗左侧那根线：窗口自己滚（`setReasoningWindow` 里 scrollTop=scrollHeight），
# 线要**不跟着滚走**。直接造一扇同类名的窗、灌满、滚到底，再数像素——比等实况里
# 那一瞬间稳得多，而且验的是同一份样式表。
RAIL_PROBE = """() => {
  document.getElementById('railProbe')?.remove();
  const host = document.createElement('div');
  host.id = 'railProbe';
  host.className = 'reasoning-window';
  host.style.cssText = 'display:block;position:fixed;left:0;top:0;z-index:99999;width:420px';
  host.style.setProperty('--think-window-lines', '6');
  host.textContent = Array.from({ length: 40 }, (_, i) => `第 ${i + 1} 行思考内容`).join('\\n');
  document.body.appendChild(host);
  host.scrollTop = host.scrollHeight;
  const box = host.getBoundingClientRect();
  return {
    scrolled: host.scrollTop > 0,
    railX: parseFloat(getComputedStyle(host).backgroundPositionX) || 0,
    width: Math.round(box.width),
    height: Math.round(box.height),
  };
}"""

# 悬浮：鼠标落在命令行上时，抬头和命令行要一起亮（两边互相点亮）。
HOVER = """(where) => {
  const card = document.querySelector('.proc-steps .tool-card.is-command');
  const head = card.querySelector('.tool-head');
  const rows = card.querySelector('.tool-command-preview');
  const bg = (el) => getComputedStyle(el).backgroundColor;
  return { head: bg(head), rows: bg(rows) };
}"""

EXPANDED = """() => {
  const card = document.querySelector('.proc-steps .tool-card.is-command');
  if (!card) return null;
  const details = [...card.querySelectorAll('.tool-fold .tool-detail')];
  const shown = details.filter((d) => !d.hidden);
  return {
    cardClass: card.className,
    labels: shown.map((d) => {
      const label = d.querySelector('.tool-detail-label');
      return {
        text: label ? label.textContent : null,
        height: label ? Math.round(label.getBoundingClientRect().height) : 0,
      };
    }),
    firstMarked: shown.filter((d) => d.classList.contains('is-first-detail')).length,
    // 第一块之外的每块都要有一道上边界
    borders: shown.map((d) => getComputedStyle(d).borderTopWidth),
    // 收起态那几行命令在展开后让位（完整命令在「参数」里）
    previewVisibleWhenOpen: (() => {
      const p = card.querySelector('.tool-command-preview');
      return !!(p && p.getBoundingClientRect().height > 0);
    })(),
  };
}"""


def rail_covers(png_bytes, rail):
    """截图里那根竖线是不是从顶画到底。

    线是背景画的，位置在 `background-position-x`。按那一列逐行看有没有比底色深
    的像素——滚走的话顶上会缺一截（滚多少缺多少）。
    """
    import io

    try:
        from PIL import Image
    except ImportError:
        return None
    image = Image.open(io.BytesIO(png_bytes)).convert("RGB")
    # 截图是设备像素，元素宽度是 CSS 像素，按比例换算线的 x。
    scale = image.width / max(rail.get("width") or 1, 1)
    x = int(round((rail.get("railX") or 0) * scale))
    x = max(0, min(image.width - 1, x))
    background = image.getpixel((image.width - 2, image.height // 2))
    lit = 0
    for y in range(image.height):
        # 线只有 1px 宽，缩放后可能落在相邻列，左右各看一格。
        if any(
            sum(abs(a - b) for a, b in zip(image.getpixel((px, y)), background)) > 24
            for px in range(max(0, x - 1), min(image.width, x + 2))
        ):
            lit += 1
    return lit >= image.height * 0.95


def write_config(lines):
    (harness.HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-tools"}],
        "providers": [
            {
                "id": "stub",
                "display_name": "Stub",
                "base_url": f"http://127.0.0.1:{harness.STUB_PORT}/v1",
                "protocol": "openai-chat",
                "api_key": "stub",
                "models": ["stub-tools"],
                "model_context_window": {"stub-tools": 100000},
            }
        ],
        "memory": {"enabled": False},
        # 界面语言钉成中文：判定比的是「参数」「结果」（见 run.py `UI_LANGUAGE`）。
        "display": {"command_output_lines": lines, "language": harness.UI_LANGUAGE},
    }
    (harness.HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def walk(lines, command, shot_name, report, expand=False):
    """跑一整轮，回来 (收起态探针, 展开态探针)。"""
    out = harness.OUT
    for path in (harness.HOME, harness.RUNTIME):
        if path.exists():
            shutil.rmtree(path)
    harness.RUNTIME.mkdir(parents=True)
    write_config(lines)

    stub = subprocess.Popen(
        [sys.executable, str(HERE / "stub_tools.py")],
        env=dict(
            os.environ,
            STUB_PORT=str(harness.STUB_PORT),
            STUB_CMD_A=command,
            STUB_TITLE_A=TITLE,
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    daemon = None
    try:
        if not harness.wait_http(f"http://127.0.0.1:{harness.STUB_PORT}/v1/models"):
            return None, None
        daemon = subprocess.Popen(
            [str(harness.BIN), "__daemon", "--port", str(harness.PORT)],
            env=harness.ENV,
            cwd=str(out),
            stdout=(out / "daemon.log").open("w"),
            stderr=subprocess.STDOUT,
        )
        if not harness.wait_http(f"{harness.BASE}/api/health"):
            return None, None
        harness.authlib.bootstrap(harness.BASE)
        session = harness.api("POST", "/api/sessions", {"name": "命令签", "switch": True})
        sid = session.get("session_id") or session.get("session", {}).get("session_id")
        harness.api("POST", "/api/turns", {"content": "跑个命令", "session_id": sid})

        with sync_playwright() as play:
            browser = play.chromium.launch()
            page = browser.new_page(viewport={"width": 1280, "height": 900})
            errors = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.route("**/*", harness.serve_local)
            page.goto(harness.BASE)
            harness.authlib.ui_login(page)
            page.wait_for_selector(".tool-card.is-command", timeout=60000)
            try:
                page.wait_for_function(
                    "() => document.querySelectorAll('.tool-card.is-command.is-success,"
                    " .tool-card.is-command.is-failure').length > 0",
                    timeout=60000,
                )
            except Exception:
                pass
            # 等这一轮真跑完再展开：回复正文一到，时间线会被切断、整组收起
            # （app.js `procLineBreak`）。命令卡成功时正文可能还没来，这时点开会被
            # 随后那次收起盖掉，连线量出来是 0 高——09-24 连跑三遍，各场景开没开
            # 每次都不一样。整轮收尾时 `live-assistant` 摘掉，那之后不会再收。
            try:
                page.wait_for_function(
                    "() => !document.querySelector('.live-assistant')", timeout=60000
                )
            except Exception:
                pass
            # 一轮跑完整组会收成一行「Worked for …」，要看的是展开态。
            page.evaluate(
                "() => document.querySelectorAll('.proc-line:not(.is-open) .proc-head')"
                ".forEach((h) => h.click())"
            )
            time.sleep(1.0)
            # 等版式**稳定**再量：连线的重贴走 rAF + ResizeObserver，读早了会拿到
            # 中间态（实测抛出过一次「连线底 196 / 命令行底 493」的假失败）。
            # 等的是「不再变」，不是「等成期望值」——后者会把断言变成同义反复。
            settle = """() => {
              const rail = document.querySelector('.proc-line .proc-rail');
              return rail ? Math.round(rail.getBoundingClientRect().bottom) : -1;
            }"""
            stable, seen = 0, None
            for _ in range(40):
                now = page.evaluate(settle)
                stable = stable + 1 if now == seen else 0
                seen = now
                if stable >= 3:
                    break
                time.sleep(0.1)
            got = page.evaluate(PROBE)
            card = page.query_selector(".proc-steps .tool-card.is-command")
            if card:
                card.screenshot(path=str(out / f"{shot_name}.png"))
            group = page.query_selector(".proc-line")
            if group:
                group.screenshot(path=str(out / f"{shot_name}-group.png"))
            if expand:
                # ① 点命令那几行应该能把卡片展开（它们在 `.tool-head` 外面）
                before = page.evaluate(
                    "() => document.querySelector('.proc-steps .tool-card.is-command')"
                    ".classList.contains('collapsed')"
                )
                page.evaluate(
                    "() => document.querySelector("
                    "'.proc-steps .tool-card.is-command .tool-command-preview').click()"
                )
                time.sleep(0.6)
                after = page.evaluate(
                    "() => document.querySelector('.proc-steps .tool-card.is-command')"
                    ".classList.contains('collapsed')"
                )
                report["点命令那几行能展开"] = before is True and after is False
                # 收回去，后面的展开态检查从同一个起点走
                page.evaluate(
                    "() => document.querySelector("
                    "'.proc-steps .tool-card.is-command .tool-head').click()"
                )
                time.sleep(0.4)

                # ② 思考滚动窗的线：滚到底之后仍然从顶画到底
                rail = page.evaluate(RAIL_PROBE)
                shot = page.query_selector("#railProbe").screenshot()
                (out / "rail-probe.png").write_bytes(shot)
                report["思考窗真的滚起来了"] = rail.get("scrolled") is True
                report["线不随内容滚走（顶到底都在）"] = rail_covers(shot, rail)
                page.evaluate("() => document.getElementById('railProbe')?.remove()")

            opened = None
            if expand:
                page.evaluate(
                    "() => document.querySelector("
                    "'.proc-steps .tool-card.is-command .tool-head').click()"
                )
                time.sleep(0.8)
                opened = page.evaluate(EXPANDED)
                card = page.query_selector(".proc-steps .tool-card.is-command")
                if card:
                    # 整张卡比视口还高，直接 element.screenshot 会从中间截起、
                    # 把「参数 / 结果」那道分界线切掉。只要顶上那 340px。
                    # `scroll_into_view_if_needed` 对比视口还高的元素会把**底边**
                    # 滚进来，于是截到的是尾巴。要顶边，得自己 block:'start'。
                    card.screenshot(path=str(out / "command-expanded-tall.png"))
                # 分界线看那张**短**卡片（`ls /`）：长的那张比视口还高，怎么截都
                # 只能截到尾巴，而要看的恰恰是「参数 / 结果」交界那一处。
                # 第一组里的第二张命令卡（`ls /`）——同一组、同样展开着，但它短，
                # 一屏放得下，交界那一处看得清。
                page.evaluate(
                    "() => { const group = document.querySelector('.proc-line');"
                    " const card = group && group.querySelectorAll("
                    "'.tool-card.is-command')[1];"
                    " if (card && card.classList.contains('collapsed'))"
                    "   card.querySelector('.tool-head').click(); }"
                )
                time.sleep(0.6)
                short = page.query_selector(
                    ".proc-line .tool-card.is-command:nth-of-type(n) ~ .tool-card.is-command"
                )
                if short:
                    short.screenshot(path=str(out / "command-expanded.png"))
            # —— 刷新之后还得是同一套版式 ——
            # 刷新走的是**另一条**建卡片的路（从 turn.tool_flow 重建），以前只改了
            # 实时那条，于是一刷新命令就挤回抬头、展开区顶上还多一道线（用户 09-20）。
            if expand:
                page.reload()
                harness.authlib.ui_login(page)
                page.wait_for_selector(".tool-card.is-command", timeout=60000)
                page.evaluate(
                    "() => document.querySelectorAll('.proc-line:not(.is-open) .proc-head')"
                    ".forEach((h) => h.click())"
                )
                time.sleep(1.2)
                report["刷新后：命令仍排在抬头底下"] = (
                    page.evaluate(PROBE).get("previewVisible") is True
                )
                reloaded = page.evaluate(PROBE)
                report["刷新后：抬头右边仍是 short_title"] = reloaded.get("summary") == TITLE
                report["刷新后：读秒还在"] = bool(reloaded.get("seconds"))
                report["刷新后：内容加 ⋮ 一共 8 行"] = (
                    reloaded.get("capRows", 0) + (1 if reloaded.get("moreVisible") else 0) == 8
                )
                # 悬浮：命令行亮，抬头跟着亮
                page.hover(".proc-steps .tool-card.is-command .tool-command-preview")
                time.sleep(0.3)
                hov = page.evaluate(HOVER, "rows")
                page.mouse.move(0, 0)
                time.sleep(0.3)
                idle = page.evaluate(HOVER, "none")
                report["悬浮命令行会高亮"] = hov["rows"] != idle["rows"]
                report["悬浮命令行时抬头一起亮"] = hov["head"] != idle["head"]
                # 展开：顶上不该多一道线和一截空
                page.evaluate(
                    "() => document.querySelector("
                    "'.proc-steps .tool-card.is-command .tool-head').click()"
                )
                time.sleep(0.8)
                after = page.evaluate(EXPANDED)
                borders = (after or {}).get("borders") or []
                report["刷新后：第一块仍不带分界线"] = (
                    (after or {}).get("firstMarked") == 1
                    and len(borders) >= 1
                    and borders[0] == "0px"
                )
                report["刷新后：展开区仍有标签"] = all(
                    x["height"] > 0 for x in (after or {}).get("labels", [])
                ) and bool((after or {}).get("labels"))
                card = page.query_selector(".proc-line .tool-card.is-command")
                if card:
                    card.screenshot(path=str(out / "command-after-reload.png"))
                print(f"  （刷新后：标签 {[x['text'] for x in (after or {}).get('labels', [])]}，"
                      f"边框 {borders}）")

            report["页面没有报错"] = report.get("页面没有报错", True) and not errors
            browser.close()
            return got, opened
    finally:
        for process in (daemon, stub):
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()


def main():
    harness.OUT.mkdir(parents=True, exist_ok=True)
    report = {}

    # —— 一、12 行短命令，8 行档：抬头形状 + 裁剪 + ⋮ + 展开态分界 ——
    got, opened = walk(8, COMMAND, "command-step-8", report, expand=True)
    if not got or got.get("missing"):
        print("! 没抓到命令签", file=sys.stderr)
        return 2

    order = [p["cls"] for p in (got.get("titleParts") or [])]
    report["抬头顺序是 名字 → 读秒 → short_title"] = (
        "tool-command-seconds" in order
        and "tool-summary" in order
        and order.index("tool-command-seconds") < order.index("tool-summary")
    )
    report["抬头右边是 short_title 不是命令"] = got.get("summary") == TITLE
    seconds = str(got.get("seconds") or "")
    report["读秒定格成真实耗时"] = bool(seconds) and ("ms" in seconds or "s" in seconds)
    report["读秒不是 ticker 那个假数"] = seconds != "0s"

    report["命令排在抬头底下"] = got.get("previewVisible") is True
    report["第一行是命令头一行（留头不留尾）"] = bool(got.get("rows")) and got["rows"][0] == "set -euo pipefail"
    # 不变式：内容占几行 + `⋮` 那行，加起来正好是配置的那个数。
    report["内容加 ⋮ 一共 8 行"] = (
        got.get("capRows", 0) + (1 if got.get("moreVisible") else 0) == 8
    )
    report["装不下时给 ⋮"] = got.get("moreVisible") is True
    report["⋮ 和命令行同一列"] = abs(got.get("moreLeft", 0) - got.get("previewLeft", -99)) <= 1
    report["命令行缩进到文字列"] = got.get("previewLeft", 0) >= got.get("iconRight", 0)
    report["命令输出仍然不在收起态露"] = got.get("outputPreviewVisible") is False
    rail = got.get("rail") or {}
    report["末尾那段命令行旁边也有线"] = (
        bool(rail) and rail.get("railBottom", 0) >= rail.get("lastRowsBottom", 1) - 2
    )
    print(f"  （连线底 {rail.get('railBottom')} / 末尾命令行底 {rail.get('lastRowsBottom')}）")
    print(f"  （8 行档：读秒 {seconds!r}，内容 {got.get('capRows')} 行 + ⋮ "
          f"{got.get('moreVisible')}，行高 {got.get('lineHeight')}px）")

    labels = [x["text"] for x in (opened or {}).get("labels", [])]
    heights = [x["height"] for x in (opened or {}).get("labels", [])]
    report["展开后每块都有标签"] = bool(labels) and all(h > 0 for h in heights)
    report["标签就是「参数」和「结果」"] = labels == ["参数", "结果"]
    report["只有第一块不带分界线"] = (opened or {}).get("firstMarked") == 1
    borders = (opened or {}).get("borders") or []
    report["第二块起有分界线"] = len(borders) >= 2 and borders[0] == "0px" and all(
        b != "0px" for b in borders[1:]
    )
    report["展开后收起态那几行让位"] = (opened or {}).get("previewVisibleWhenOpen") is False
    print(f"  （展开态：标签 {labels}，边框 {borders}）")

    # —— 二、一条超长单行：要软换行填满，不是一行省略号 ——
    long_got, _ = walk(8, LONG, "command-step-long", report)
    spans = (long_got or {}).get("rowSpans") or []
    report["长单行会软换行"] = bool(spans) and spans[0] > 1
    report["长单行把空间用满（不止一行）"] = (spans[0] if spans else 0) > 1
    long_total = (long_got or {}).get("capRows", 0) + (
        1 if (long_got or {}).get("moreVisible") else 0
    )
    report["长单行也不超过 8 行"] = 0 < long_total <= 8
    print(f"  （长单行：折成 {spans[0] if spans else 0} 行，容器 "
          f"{(long_got or {}).get('capRows')} 行，⋮ {(long_got or {}).get('moreVisible')}）")

    # —— 三、换档位：3 行 / 0 行 ——
    got3, _ = walk(3, COMMAND, "command-step-3", report)
    report["改成 3 行就一共 3 行"] = (got3 or {}).get("capRows", 0) + (
        1 if (got3 or {}).get("moreVisible") else 0
    ) == 3
    report["3 行档也有 ⋮"] = (got3 or {}).get("moreVisible") is True

    got0, _ = walk(0, COMMAND, "command-step-0", report)
    report["0 行就一行都不露"] = (got0 or {}).get("previewVisible") is False
    report["0 行时抬头还在"] = (got0 or {}).get("summary") == TITLE

    (harness.OUT / "command-rows.json").write_text(
        json.dumps(
            {"8": got, "expanded": opened, "long": long_got, "3": got3, "0": got0,
             "report": report},
            ensure_ascii=False, indent=2,
        ),
        encoding="utf-8",
    )
    passed = sum(1 for ok in report.values() if ok)
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{passed}/{len(report)} passed  产物在 {harness.OUT}")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""三个显示开关的真机走查（09-17 用户拍板的那一版语义）。

设置里只剩三位布尔，互不相干：

- `展开思考内容` / `展开工具内容`：开着的话那一步**出来就是展开的**，不用点；
  再点一次收回去。关着就只有抬头。
- `过程收起成 Worked for`：只管收不收段。关掉的话每一步就地留着，**照样点得开**。

单元测试钉的是字节（`miyu-block-open=` 那个标记）和视图映射；这份钉的是**人眼
看到的那一屏**——标记对了但 `paint` 没去开、或者开了又被下一帧顶回去，字节那层
一个都照不出来。

    cargo build
    python3 testkit/tui/expand_switches.py

**这些 TUI 走查只能一个一个跑**：它们共用同一个 `MIYU_HOME`（`/tmp/miyu-tui-smoke/home`）
和同一个桩模型端口，起头还会 `rmtree` 那个家目录。并行跑的话两边互相掀桌子，红成一片
而代码一点问题都没有。
"""

import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

# 第一段思考的正文（桩模型写死的那句，见 `repl-smoke/stub_llm.py`）。
# 它只在**展开之后**才看得到——抬头上只有「已思考 · N 词元 · 1.2s」。
THINK_BODY = "先想一句"
THOUGHT_HEAD = "已思考"
FOLD_HEAD = "Worked for"
# 「正在想」那一段正文里的记号：命令之后那一段思考，逐块流出来的那份。
THINK_LIVE = "折叠时看不到"

STUB = {
    "STUB_REASONING": "1",
    "STUB_TOOL": "1",
    "STUB_TOOL_COMMAND": "printf 'out-%s\\n' one",
    "STUB_CHUNK_SLEEP": "0.02",
}


def display(**flags):
    return {"display": flags}


def row_of(screen, needle):
    for index, line in enumerate(screen):
        if needle in line:
            return index
    return None


def ask(master, sink):
    os.write(master, h.PROMPT.encode())
    h.drain_until(master, sink, h.PROMPT, 3.0)
    os.write(master, b"\r")
    h.settle(master, sink, quiet=1.2, timeout=40)
    return h.render(bytes(sink))


def follow_while(master, sink, alive, wanted, seconds=4):
    """跟着帧走，直到看见 `wanted`；`alive` 从屏上消失就停（那一步已经过去了）。"""
    deadline = time.time() + seconds
    best = h.render(bytes(sink))
    while time.time() < deadline:
        h.settle(master, sink, quiet=0.15, timeout=1)
        rows = h.render(bytes(sink))
        if not any(alive in line for line in rows):
            break
        best = rows
        if any(wanted in line for line in rows):
            break
    return best


def screen_after(master, sink, quiet=0.6, timeout=8):
    h.settle(master, sink, quiet=quiet, timeout=timeout)
    return h.render(bytes(sink))


def scenario_expanded(report):
    """两个「展开」开关都开着：那一步出来就是展开的，点一下收回去。

    这一档要和「收起成 Worked for」分开看——那是另一位开关。这里把它关掉，
    好让那几步留在屏上，断言才不用去数收缩行里面。
    """
    stub, daemon, tui, master, sink = r.start(
        STUB,
        config_extra=display(
            expand_reasoning=True, expand_tool_calls=True, fold_timeline=False
        ),
    )
    try:
        screen = ask(master, sink)
        r.save("expand-open", screen)
        # 一次都没点，思考正文就该在屏上。
        report["expanded_body_visible_without_a_click"] = any(
            THINK_BODY in line for line in screen
        )
        head = row_of(screen, THOUGHT_HEAD)
        report["expanded_head_present"] = head is not None
        if head is None:
            return
        # 再点一次收回去——它是把手，不是一截死预览。
        h.click(master, sink, 5, head)
        screen = screen_after(master, sink)
        r.save("expand-collapsed-again", screen)
        report["clicking_the_head_collapses_it"] = not any(
            THINK_BODY in line for line in screen
        )
        # 收起来之后不许被下一帧顶开（活动区每 tick 重写同样的标记）。
        screen = screen_after(master, sink, quiet=1.0)
        report["stays_collapsed_across_frames"] = not any(
            THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_expanded_survives_the_fold(report):
    """收成 `Worked for …` 之后再点开：里面那几步**还是**展开态。

    收缩行里的那份原来是另写的一套拼行，不走 `step_rows`——这一位（以及命令那
    一步抬头底下露着的几行）一收段就没了。
    """
    stub, daemon, tui, master, sink = r.start(
        STUB,
        config_extra=display(expand_reasoning=True),
    )
    try:
        screen = ask(master, sink)
        fold = row_of(screen, FOLD_HEAD)
        report["folded_even_with_expand_on"] = fold is not None
        if fold is None:
            return
        # 段是收着的，所以这会儿屏上不该有思考正文。
        report["fold_hides_the_expanded_body"] = not any(
            THINK_BODY in line for line in screen
        )
        h.click(master, sink, 3, fold)
        screen = screen_after(master, sink)
        r.save("expand-fold-opened", screen)
        # 点开收缩行**就够了**：里面那一步不用再点。
        report["step_inside_the_fold_is_still_open"] = any(
            THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_collapsed(report):
    """出厂档位：那一步出来是合着的，点开才看得到。"""
    stub, daemon, tui, master, sink = r.start(
        STUB,
        config_extra=display(expand_reasoning=False, expand_tool_calls=False),
    )
    try:
        screen = ask(master, sink)
        r.save("expand-default", screen)
        report["collapsed_body_hidden_by_default"] = not any(
            THINK_BODY in line for line in screen
        )
        # 收成 `Worked for …` 了，得先点开它才看得到那几步。
        fold = row_of(screen, "Worked for")
        report["default_folds_the_segment"] = fold is not None
        if fold is None:
            return
        h.click(master, sink, 3, fold)
        screen = screen_after(master, sink)
        head = row_of(screen, THOUGHT_HEAD)
        if head is None:
            report["collapsed_step_expands_on_click"] = False
            return
        h.click(master, sink, 5, head)
        screen = screen_after(master, sink)
        r.save("expand-default-clicked", screen)
        report["collapsed_step_expands_on_click"] = any(
            THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_no_fold(report):
    """关掉「过程收起成 Worked for」：不收段，但每一步**照样点得开**。

    09-17 之前这一档顺手把那些步变成点不开、正文铺一地——用户原话：「即使不自动
    收起过程为 true，也不应该以 tag 行下预览的形式出现 tag 行的内容」。
    """
    stub, daemon, tui, master, sink = r.start(
        STUB,
        config_extra=display(fold_timeline=False),
    )
    try:
        screen = ask(master, sink)
        r.save("expand-nofold", screen)
        report["no_fold_line"] = not any("Worked for" in line for line in screen)
        report["no_fold_body_not_spilled"] = not any(
            THINK_BODY in line for line in screen
        )
        head = row_of(screen, THOUGHT_HEAD)
        report["no_fold_step_present"] = head is not None
        if head is None:
            return
        h.click(master, sink, 5, head)
        screen = screen_after(master, sink)
        r.save("expand-nofold-clicked", screen)
        report["no_fold_step_still_clickable"] = any(
            THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_live_is_expanded_too(report):
    """**还在想 / 还在跑**的时候就该是展开的，不是想完了才展开。

    用户 09-17 实测：「思考的展开是思考完成才展开，思考完成之前还是单行窥视的
    状态；运行命令工具运行中是渲染 tag 行下预览，运行完成才展开」。根因是 live
    区那几行用的是不带档位的起始标记——一步的大半辈子都在 live 区里。
    """
    stub, daemon, tui, master, sink = r.start(
        # 慢一点，好在"还在想""还在跑"的那几帧上做断言。
        dict(STUB, STUB_CHUNK_SLEEP="0.3", STUB_TOOL_COMMAND="sleep 2; printf 'out\\n'"),
        config_extra=display(expand_reasoning=True, expand_tool_calls=True),
    )
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        # 等到「思考中」那一行出现**并且**它底下已经有正文——这两件事该在同一帧。
        # 先看跑着的命令那一行。
        screen = r.wait_screen(
            master,
            sink,
            lambda rows: any("运行命令" in line for line in rows),
            timeout=30,
        )
        report["live_command_row_seen"] = screen is not None
        if screen is None:
            return
        screen = follow_while(master, sink, "运行命令", "sleep 2", seconds=3)
        r.save("expand-live-command", screen)
        # 展开态下命令那一步露的是**缩进的**正文，不是 `│ ` 那条预览尾巴。
        report["live_command_is_expanded"] = any(
            line.startswith("    sleep 2") for line in screen
        )
        # 再看命令之后那一段思考（这一段是逐块流出来的，看得见"正在想"）。
        screen = r.wait_screen(
            master,
            sink,
            lambda rows: any("思考中" in line for line in rows),
            timeout=30,
        )
        report["live_thinking_row_seen"] = screen is not None
        if screen is None:
            return
        # 刚冒头那一帧一个字都还没来，那一块根本没登记（没内容可展开）。
        # 再跟几帧，要求「思考中」还在——只在它还活着的那几帧上断言。
        screen = follow_while(master, sink, "思考中", THINK_LIVE, seconds=6)
        r.save("expand-live-thinking", screen)
        report["live_thinking_is_expanded"] = any(THINK_LIVE in line for line in screen)
    finally:
        r.stop(tui, daemon, stub)


def scenario_window_when_collapsed(report):
    """「展开思考内容」关着：正在想时抬头底下开一扇窗，滚着露最近几行；想完收成
    一行 `已思考 · …`（用户 09-17：「思考行不是单行窥视，而是有滚动」）。窗多高
    按 `display.thinking_scroll_lines`。
    """
    stub, daemon, tui, master, sink = r.start(
        # 慢一点，好在"还在想"的那几帧上做断言。
        dict(STUB, STUB_CHUNK_SLEEP="0.3"),
        config_extra=display(expand_reasoning=False, thinking_scroll_lines=3),
    )
    try:
        os.write(master, h.PROMPT.encode())
        h.drain_until(master, sink, h.PROMPT, 3.0)
        os.write(master, b"\r")
        # 命令之后那一段思考是逐块流出来的，看得见"正在想"。
        screen = r.wait_screen(
            master,
            sink,
            lambda rows: any("运行命令" in line for line in rows),
            timeout=30,
        )
        screen = r.wait_screen(
            master,
            sink,
            lambda rows: any("思考中" in line for line in rows),
            timeout=30,
        )
        report["window_thinking_row_seen"] = screen is not None
        if screen is None:
            return
        screen = follow_while(master, sink, "思考中", THINK_LIVE, seconds=6)
        r.save("window-live", screen)
        head = row_of(screen, "思考中")
        rows = []
        if head is not None:
            j = head + 1
            while j < len(screen) and screen[j].startswith("  │"):
                rows.append(screen[j])
                j += 1
        report["window_rows_under_the_heading"] = any(THINK_LIVE in line for line in rows)
        report["window_is_at_most_three_rows"] = 0 < len(rows) <= 3
        # 抬头后面不再挂单行窥视：正文在窗里。
        report["heading_has_no_peek"] = head is not None and THINK_LIVE not in screen[head]
        h.settle(master, sink, quiet=1.2, timeout=40)
        screen = h.render(bytes(sink))
        r.save("window-done", screen)
        # 想完那一步收成一行、照旧收进 `Worked for` 里：屏上不该再有正文。
        report["window_collapses_to_a_tag"] = not any(
            THINK_LIVE in line or THINK_BODY in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def scenario_config_applies_next_turn(report):
    """`/config` 改完，**下一轮**就生效，不用重开 TUI。"""
    stub, daemon, tui, master, sink = r.start(
        STUB, config_extra=display(expand_reasoning=False, fold_timeline=False)
    )
    path = h.HOME / "config" / "config.jsonc"
    try:
        screen = ask(master, sink)
        report["before_config_is_collapsed"] = not any(
            THINK_BODY in line for line in screen
        )
        # 主菜单里进「全局参数设置」，表里「展开思考内容」选「启用」，再回主菜单选
        #「保存并退出」。要按几下 ↓ 按屏上的菜单现数：原来写死「第 9 项 / 第 8 项」，
        # 菜单精简掉一项之后就点进了「语音功能」（09-24 查实，纯 main 上也红）。
        os.write(master, b"/config")
        h.drain_until(master, sink, "/config", 3.0)
        os.write(master, b"\r")
        h.settle(master, sink, quiet=0.8, timeout=15)

        def press(keys):
            os.write(master, keys.encode("latin1"))
            h.settle(master, sink, quiet=0.4, timeout=10)
            return h.render(bytes(sink))

        def downs(screen, first, target):
            """同一张菜单里从 `first` 那一项走到 `target` 要按几下 ↓（一项一行）。"""
            top, goal = row_of(screen, first), row_of(screen, target)
            return 0 if top is None or goal is None else goal - top

        screen = h.render(bytes(sink))
        screen = press("\x1b[B" * downs(screen, "供应商和模型", "全局参数设置") + "\r")
        screen = press("\x1b[B" * downs(screen, "界面语言", "展开思考内容") + "\r")
        screen = press("\x1b[A")
        screen = press("\r")
        screen = press("\x1b")
        press("\x1b[B" * downs(screen, "全局参数设置", "保存并退出") + "\r")
        saved = json.loads(path.read_text(encoding="utf-8"))
        report["config_tui_saved_it"] = saved.get("display", {}).get("expand_reasoning") is True
        os.write(master, "再来一句".encode())
        h.drain_until(master, sink, "再来一句", 5.0)
        os.write(master, b"\r")
        screen = screen_after(master, sink, quiet=1.5, timeout=40)
        r.save("expand-after-config", screen)
        # 第二轮只有一段思考，用它那份正文当记号。
        report["takes_effect_on_the_next_turn"] = any(
            "折叠时看不到" in line for line in screen
        )
    finally:
        r.stop(tui, daemon, stub)


def main():
    report = {}
    for scenario in (
        scenario_live_is_expanded_too,
        scenario_config_applies_next_turn,
        scenario_expanded,
        scenario_expanded_survives_the_fold,
        scenario_collapsed,
        scenario_no_fold,
        scenario_window_when_collapsed,
    ):
        try:
            scenario(report)
        except Exception as error:  # noqa: BLE001 - 走查脚本，报出来就行
            report[f"{scenario.__name__}_crashed"] = False
            print(f"{scenario.__name__} 炸了: {error}", file=sys.stderr)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    bad = [key for key, value in report.items() if value is not True]
    print(f"\n{len(report) - len(bad)}/{len(report)} 通过")
    if bad:
        print("红:", bad)
    print(f"产物：{h.OUT}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())

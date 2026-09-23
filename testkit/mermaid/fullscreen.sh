#!/usr/bin/env bash
# 全屏 TUI 里的 ```mermaid：借 testkit/tui/kitty_shot.py 的台架跑一遍。
#
# 为什么还要这一趟：terminal.py 验的是 inline 形态（`miyu "…"` 一次性输出），
# 而日常用的是**全屏 TUI**——那边正文是从缓冲重建的，图靠 kitty 的占位格活着。
# 「图画出来了」和「重画一帧之后图还在」是两件事，后者只有全屏下才存在。
#
#     cargo build
#     testkit/kitty-image/run_headless.sh testkit/mermaid/fullscreen.sh
#
# 看 $OUT/tui-table-math.png（第二轮那张，正文里就是这段 mermaid）。
set -u
# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for __herdr_var in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$__herdr_var"; done
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export OUT="${OUT:-$HOME/.cache/miyu-mermaid-tui}"
export BIN="${BIN:-$REPO/target/debug/miyu}"
# MERMAID_LONG=1 换成一张竖着很长的图：验「长图不再被压扁」那一条。
if [ -n "${MERMAID_LONG:-}" ]; then
    body="flowchart TD"
    for i in $(seq 0 11); do
        body="$body
    N$i[步骤$i 做一件事] --> N$((i + 1))[步骤$((i + 1)) 做另一件事]"
    done
else
    body='flowchart TD
    A[用户提问] --> B{要用工具吗}
    B -->|要| C[调用工具]
    B -->|不要| D[直接回答]
    C --> D'
fi
export SHOT_TEXT="这是全屏下的图表：

\`\`\`mermaid
$body
\`\`\`

图下面还得有正文。"
# 图下面那句正文之后再逼它重画一帧：占位格真进了缓冲，图才还在。
export SHOT_REPAINT_LAST=1
# 真点一下「点开看大图」，并用一个**假的 xdg-open** 接住，看它到底开了什么。
# （不放假的话点下去会真的弹出看图器。）
export SHOT_CLICK_TEXT="点开看大图"
FAKE="$OUT/fakebin"
mkdir -p "$FAKE"
OPENED="$OUT/opened.txt"
: >"$OPENED"
printf '#!/bin/sh\nprintf "%%s\\n" "$1" >> "%s"\n' "$OPENED" >"$FAKE/xdg-open"
cp "$FAKE/xdg-open" "$FAKE/open"          # macOS 那条也顺手接住
chmod +x "$FAKE/xdg-open" "$FAKE/open"
export PATH="$FAKE:$PATH"
python3 "$REPO/testkit/tui/kitty_shot.py"
rc=$?

# 「点开看大图」那行得真的落进全屏的正文缓冲——kitty_shot 把屏幕文字存了一份。
# （链接目标在 OSC 8 里，屏幕文字看不见；那一段由单测 clicking_a_link_finds_its_target
# 和 probe.py 各守一头。）
pass=0; total=0
check() {
    total=$((total + 1))
    if [ "$1" = ok ]; then pass=$((pass + 1)); echo "✅ $2"; else echo "❌ $2"; fi
}
grep -q "点开看大图" "$OUT/screen.txt" 2>/dev/null \
    && check ok "全屏正文里有「点开看大图」那行" \
    || check no "全屏正文里有「点开看大图」那行"
opened=$(head -1 "$OPENED" 2>/dev/null)
case "$opened" in
    file://*.svg) check ok "点下去真的开了那张 SVG（$opened）" ;;
    "") check no "点下去什么都没开（这就是用户报的「没有任何反应」）" ;;
    *) check no "点下去开了别的东西：$opened" ;;
esac
[ -f "${opened#file://}" ] && check ok "开的那个文件真存在" || check no "开的那个文件不存在"
echo "$pass/$total passed"
[ "$pass" = "$total" ] || rc=1
exit $rc

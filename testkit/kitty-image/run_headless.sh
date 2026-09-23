#!/usr/bin/env bash
# 在无头 cage(wlroots headless 后端)里起一个真 kitty 跑指定命令,不碰用户桌面。
# kitty 带远程控制(KITTY_LISTEN_ON),里面的脚本可以用 `kitten @ action ...` 滚视口,
# 用 grim 截图(截的是无头输出)。
#
# 用法:
#     testkit/kitty-image/run_headless.sh python3 testkit/kitty-image/ghost_probe.py
#     OUT=~/.cache/miyu-kitty-probe testkit/kitty-image/run_headless.sh <cmd...>
set -u
# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for __herdr_var in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$__herdr_var"; done
export OUT="${OUT:-$HOME/.cache/miyu-kitty-probe}"
mkdir -p "$OUT"
export WLR_BACKENDS=headless
export WLR_LIBINPUT_NO_DEVICES=1
export WLR_RENDERER="${WLR_RENDERER:-pixman}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
unset DISPLAY
unset WAYLAND_DISPLAY
# 无头输出的尺寸:大一点好放下 Miyu 的活动区。
export WLR_HEADLESS_OUTPUTS=1
exec cage -- kitty \
    -o allow_remote_control=yes \
    -o font_size="${KITTY_FONT_SIZE:-11}" \
    -o scrollback_lines=2000 \
    --listen-on "unix:@miyu-kitty-probe-$$" \
    "$@"

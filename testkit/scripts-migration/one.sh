#!/usr/bin/env bash
# 单件直调:BIN=<miyu> bash one.sh <tool> '<json>'
set -u
# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for __herdr_var in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$__herdr_var"; done
BIN=${BIN:-target/release/miyu}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${OUT:-$HOME/.cache/miyu-scripts-migration}
mkdir -p "$OUT/home"
export MIYU_HOME=$OUT/home
# 隔离 IPC socket:否则工具桥会连上本机真 daemon,列的是它的会话工具面
mkdir -p "$OUT/runtime"; export XDG_RUNTIME_DIR=$OUT/runtime
export MIYU_SYSTEM_SCRIPTS_DIR=$REPO/src/scripts
"$BIN" --help 2>&1 | grep -i "tool" | head -3
echo "--- calling: $1"
timeout 60 "$BIN" tool-call "$1" "$2" 2>&1 | head -${LINES_MAX:-20}
echo "exit=${PIPESTATUS[0]}"

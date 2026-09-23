#!/bin/bash
# Homebrew formula 在一台真 Mac 上的端到端验收（09-23，Homebrew 渠道）。
#
# 用法（在 Mac 上，用系统自带的 /bin/bash 3.2 跑）：
#   mac_brew_check.sh <tar.gz> <formula.rb> [provider.json]
#   formula.rb 用 testkit/homebrew/render_local_formula.py 渲染，url 指向 file://<tar.gz>；
#   provider.json 可选：带一个 opencodego provider 的完整 config.jsonc（chmod 600），
#   给了才测 zsh 里的真对话，用完立刻删。
#
# 会往本机 Homebrew 装 miyu 和它缺的依赖；结束时只卸本次新装的（按装前的
# `brew list` 比对），不跑 autoremove，最后核对 brew list 与装前一致。关了自动更新，
# 但 brew 仍会顺手升级**过期的间接依赖**（09-23 实测把 cairo 升了一版），会打出来。
# 全程用假 HOME / MIYU_HOME，不碰本机的 rc 文件和 ~/.miyu。
#
# 注意 ssh 里用 `bash -s <<EOF` 喂脚本时，brew 会吃掉 stdin 把后面的命令读走——
# 所以脚本里每条 brew / miyu 命令都接 </dev/null。
set -u
TARBALL=$1
FORMULA=$2
PROVIDER=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
[ -f "$TARBALL" ] || { echo "找不到包: $TARBALL"; exit 2; }
grep -q "^  url \"file://$TARBALL\"$" "$FORMULA" || { echo "formula 的 url 没指向 $TARBALL"; exit 2; }
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ANALYTICS=1
export HOMEBREW_NO_ENV_HINTS=1 HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK=1
export PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin
TAP=miyu-verify/local
T=$(mktemp -d /tmp/mh-check.XXXXXX)   # 短路径：socket 受 SUN_LEN(104) 限制
SANDBOX=$T/sh
mkdir -p "$SANDBOX/home" "$SANDBOX/miyu/config" "$SANDBOX/rt"
pass=0; fail=0
check() {
  if [ "$2" = "$3" ]; then echo "PASS $1"; pass=$((pass+1)); else echo "FAIL $1: got [$2] want [$3]"; fail=$((fail+1)); fi
}
contains() {  # contains <名字> <文本> <子串>
  case "$2" in *"$3"*) check "$1" yes yes ;; *) check "$1" "$(printf '%s' "$2" | head -c 160)" "…$3…" ;; esac
}
run() {  # 隔离环境跑一条命令;PATH 由调用方给
  local path=$1; shift
  env -i PATH="$path" HOME="$SANDBOX/home" MIYU_HOME="$SANDBOX/miyu" XDG_RUNTIME_DIR="$SANDBOX/rt" \
    LANG=C.UTF-8 TERM=dumb RUST_LOG=error SHELL="${RUN_SHELL:-/bin/zsh}" "$@" </dev/null 2>&1
}
brew list --formula -1 </dev/null | sort > "$T/before.txt"

cleanup() {
  run /usr/bin:/bin /opt/homebrew/bin/miyu daemon stop >/dev/null 2>&1
  rm -f "$SANDBOX/miyu/config/config.jsonc"
  brew uninstall --formula "$TAP/miyu" </dev/null >/dev/null 2>&1
  brew untap "$TAP" </dev/null >/dev/null 2>&1
  brew untrust --formula "$TAP/miyu" </dev/null >/dev/null 2>&1
  brew list --formula -1 </dev/null | sort > "$T/installed.txt"
  added=$(comm -13 "$T/before.txt" "$T/installed.txt" | tr '\n' ' ')
  if [ -n "$added" ]; then
    for f in $added; do cache=$(brew --cache "$f" </dev/null 2>/dev/null); [ -n "$cache" ] && rm -f "$cache"; done
    # shellcheck disable=SC2086
    brew uninstall --formula $added </dev/null >/dev/null 2>&1
  fi
  brew list --formula -1 </dev/null | sort > "$T/after.txt"
  if cmp -s "$T/before.txt" "$T/after.txt"; then check "brew list 恢复到装前" same same
  else check "brew list 恢复到装前" "$(diff "$T/before.txt" "$T/after.txt" | tr '\n' ' ')" same; fi
  check "测试 tap 已移除" "$(brew tap </dev/null | grep -c "^$TAP$")" 0
  # 测试 daemon 的命令行里没有沙箱路径(MIYU_HOME 在环境里),按程序路径认。
  check "没有残留的 brew miyu daemon" "$(pgrep -f "^/opt/homebrew/bin/miyu __daemon" | wc -l | tr -d ' ')" 0
  rm -rf "$T"
  echo "RESULT pass=$pass fail=$fail"
  [ "$fail" -eq 0 ]
}
trap 'cleanup; exit $?' EXIT

brew tap-new --no-git "$TAP" </dev/null >/dev/null 2>&1
mkdir -p "$(brew --repository "$TAP" </dev/null)/Formula"
cp "$FORMULA" "$(brew --repository "$TAP" </dev/null)/Formula/miyu.rb"
brew install "$TAP/miyu" </dev/null > "$T/install.log" 2>&1
check "brew install" "$?" 0
upgraded=$(sed -n '/Would upgrade/,/^==> Fetching/p' "$T/install.log" | grep -v '^==>' | tr '\n' ' ')
[ -n "$upgraded" ] && echo "NOTE brew 顺手升级了过期的间接依赖: $upgraded"
brew test "$TAP/miyu" </dev/null > "$T/test.log" 2>&1
check "brew test" "$?" 0

VERSION=$(sed -n 's/^  version "\(.*\)"$/\1/p' "$FORMULA")
KEG=$(cd "$(brew --prefix "$TAP/miyu" </dev/null)" && pwd -P)
LINKED=/opt/homebrew/bin/miyu
printf '%s' '{"config_version":3,"oobe_done":true,"active_provider":"distribution-mock","active_provider_models":[{"provider_id":"distribution-mock","model":"mock"}],"providers":[{"id":"distribution-mock","display_name":"Local test","base_url":"http://127.0.0.1:9/v1","protocol":"openai-chat","api_key":"local-test-only","models":["mock"]}],"memory":{"enabled":false}}' > "$SANDBOX/miyu/config/config.jsonc"

check "经 /opt/homebrew/bin 启动的版本" "$(run /usr/bin:/bin $LINKED --version)" "miyu $VERSION"
personas=$(run /usr/bin:/bin $LINKED paths | sed -n 's/^system persona resources: //p')
check "系统人格资源目录落在 keg 里" "$(cd "$personas" 2>/dev/null && pwd -P)" "$KEG/share/miyu/personas"
embed=$(run /usr/bin:/bin $LINKED embed status)
contains "语义检索用 brew 的 onnxruntime" "$embed" "runtime library: /opt/homebrew/"
contains "语义检索可用" "$embed" "semantic search: available"
for skill in "$KEG"/share/miyu/personas/*/skills/*/SKILL.md; do
  name=$(basename "$(dirname "$skill")")
  contains "内置技能 $name" "$(run /usr/bin:/bin $LINKED tool-call load_skill "{\"name\":\"$name\"}")" "name=\"$name\" source=\"built_in\""
done
contains "出厂脚本 scientific_calculator" "$(run /usr/bin:/bin $LINKED tool-call scientific_calculator '{"expression":"6*7"}')" '42'
# PATH 里没有 /opt/homebrew/bin:miyu 靠绝对路径启动,也得找得到 brew 装的 rg(09-23 修的缺口)。
rg_out=$(run /usr/bin:/bin $LINKED tool-call grep "{\"pattern\":\"distribution-mock\",\"path\":\"$SANDBOX/miyu/config\"}")
contains "PATH 没有 brew 时 grep 仍找得到 rg" "$rg_out" "config.jsonc"
bash_note=$(RUN_SHELL=/bin/bash run /usr/bin:/bin $LINKED bash-init)
contains "bash-init 对 bash 3.2 给出说明" "$bash_note" "bash 3.2"
run /usr/bin:/bin $LINKED zsh-init >/dev/null
check "zsh 兜底找到 brew 的 miyu" "$(env -i PATH=/usr/bin:/bin HOME="$SANDBOX/home" MIYU_HOME="$SANDBOX/miyu" zsh -i -c 'miyu --version' </dev/null 2>/dev/null | tail -1)" "miyu $VERSION"
check "bash 兜底找到 brew 的 miyu" "$(env -i PATH=/usr/bin:/bin HOME="$SANDBOX/home" MIYU_HOME="$SANDBOX/miyu" bash -l -i -c 'miyu --version' </dev/null 2>/dev/null | tail -1)" "miyu $VERSION"

if [ -n "$PROVIDER" ]; then
  cp "$PROVIDER" "$SANDBOX/miyu/config/config.jsonc" && chmod 600 "$SANDBOX/miyu/config/config.jsonc"
  /usr/bin/python3 "$HERE/hook_dialogue.py" "$SANDBOX" zsh "$T/zsh.txt" </dev/null
  check "zsh 里打中文拿到模型回复" "$?" 0
  daemon=$(pgrep -fl "__daemon" | grep -c "^[0-9]* $LINKED __daemon")
  check "回话的 daemon 是 brew 装的那个" "$([ "$daemon" -ge 1 ] && echo yes)" yes
  run /usr/bin:/bin $LINKED daemon stop >/dev/null 2>&1
  rm -f "$SANDBOX/miyu/config/config.jsonc"
  check "provider 配置已删" "$([ -e "$SANDBOX/miyu/config/config.jsonc" ] && echo exists || echo gone)" gone
fi

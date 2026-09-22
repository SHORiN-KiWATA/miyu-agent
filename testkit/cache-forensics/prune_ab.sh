#!/usr/bin/env bash
# 工具输出剪枝对前缀缓存的代价：A/B 真模型实测。
#
# 假设：`context.tool_result_prune_chars` 在回合**落库时**把大工具输出改写成
# 「头 + 省略标记 + 尾」，而活体那一轮发出去的是全文。于是下一轮回放的历史
# 在每个被剪的工具输出处都与上游缓存里的不同，前缀断在第一个被剪的位置。
# `pruning.rs` 的注释称它「costs no cache reset at all」——这一版就是来判这句
# 话真假的。
#
#   A 组：tool_result_prune_chars = 8192（现状默认）
#   B 组：tool_result_prune_chars = 0    （关掉剪枝）
#
# 两组各起一个独立 MIYU_HOME 的沙盒 daemon（只搬供应商与模型档位，不带账号、
# 不接平台、不挂 MCP），跑同一串会产生大工具输出的提示词，再比 cache-usage
# 里的命中率与 same/prev 指纹。
#
#   bash testkit/cache-forensics/prune_ab.sh run   [轮数]
#   bash testkit/cache-forensics/prune_ab.sh report
#   bash testkit/cache-forensics/prune_ab.sh stop
set -euo pipefail

BIN=${MIYU_BIN:-$(cd "$(dirname "$0")/../.." && pwd)/target/debug/miyu}
MODEL=${MIYU_AB_MODEL:-opencodego/mimo-v2.6-flash}
REAL=$HOME/.miyu/config/config.jsonc
ROUNDS=${2:-6}

declare -A PORTS=([a]=8391 [b]=8392)
declare -A PRUNE=([a]=8192 [b]=0)

home_for() { echo "$HOME/.cache/miyu-prune-ab-$1"; }

seed() {
  local arm=$1 dir
  dir=$(home_for "$arm")
  mkdir -p "$dir/config"
  chmod 700 "$dir"
  python3 - "$REAL" "$dir/config/config.jsonc" "${PRUNE[$arm]}" <<'PY'
import json, pathlib, re, sys

real, out, prune = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), int(sys.argv[3])
cfg = json.loads(re.sub(r"^\s*//.*$", "", real.read_text(), flags=re.M))
carry = [
    "providers",
    "active_provider",
    "active_provider_models",
    "model_tiers",
    "prompt",
    "context",
    "tools",
]
seeded = {key: cfg[key] for key in carry if key in cfg}
seeded["config_version"] = cfg.get("config_version")
seeded["oobe_done"] = True
# 记忆/表情包关掉：旁路请求会混进 cache-usage，且记忆整理每轮内容都不同，
# 本身就是噪声源。
seeded["memory"] = {"enabled": False}
# 唯一自变量。其余水位保持与真配置一致，免得 trim/compact 在 A/B 间不对称。
seeded.setdefault("context", {})["tool_result_prune_chars"] = prune
# 裁剪与压缩都推到够不着的水位：这一版只问剪枝一件事。
seeded["context"]["trim_at_ratio"] = 0.99
seeded["context"]["compact_force_ratio"] = 0.99
out.write_text(json.dumps(seeded, ensure_ascii=False, indent=2), encoding="utf-8")
print(f"  {out}  prune={prune}")
PY
  chmod 600 "$dir/config/config.jsonc"
}

start() {
  local arm=$1 dir
  dir=$(home_for "$arm")
  MIYU_HOME="$dir" "$BIN" daemon --port "${PORTS[$arm]}" start >/dev/null
  echo "  [$arm] daemon :${PORTS[$arm]}  home=$dir"
}

stop_all() {
  for arm in a b; do
    local dir
    dir=$(home_for "$arm")
    MIYU_HOME="$dir" "$BIN" daemon stop >/dev/null 2>&1 || true
  done
  echo "两组 daemon 已停"
}

# 每轮都逼出一段远超 8192 字符的工具输出：剪枝的触发条件就是它。
#
# 措辞要把退路堵死。第一版写「说明输出有多长」，模型直接在命令里加了
# `| wc -c`，工具结果只有 `8893` 四个字符——阈值根本够不着，整组 A/B 白跑。
prompt_for() {
  local index=$1
  echo "调用 run_command 执行这条命令（原样执行，命令里不许加 wc/head/tail/grep 或任何截断与管道统计）：seq 1 ${index}000 | paste -sd, -  。拿到完整输出后，只回复「第 ${index} 轮完成」六个字，不要复述输出内容。"
}

run() {
  echo "== 播配置 =="
  for arm in a b; do seed "$arm"; done
  echo "== 起 daemon =="
  for arm in a b; do start "$arm"; done
  sleep 3
  # A/B 交替跑，不是一组跑完再跑另一组。第一版顺序跑，A 的冷启动请求把
  # 上游前缀缓存预热了，B 的第一条就白捡 48.5% 命中——两组差的 3.9 个
  # 百分点全是这个顺序效应，跟自变量没关系。
  echo "== 交替跑 $ROUNDS 轮（A prune=${PRUNE[a]} / B prune=${PRUNE[b]}） =="
  for i in $(seq 1 "$ROUNDS"); do
    for arm in a b; do
      local dir
      dir=$(home_for "$arm")
      MIYU_HOME="$dir" timeout 600 "$BIN" --session ab --create \
        --model "$MODEL" "$(prompt_for "$i")" >/dev/null 2>&1 || echo "    [$arm] 第 $i 轮失败"
    done
    echo "    第 $i 轮完成（两组）"
  done
  report
}

report() {
  python3 "$(dirname "$0")/prune_ab_report.py" \
    "$(home_for a)/cache/logs" "$(home_for b)/cache/logs"
}

case "${1:-run}" in
  run) run ;;
  report) report ;;
  stop) stop_all ;;
  *) echo "用法: $0 [run [轮数]|report|stop]" >&2; exit 2 ;;
esac

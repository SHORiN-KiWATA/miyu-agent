#!/usr/bin/env bash
# 水位改动的前后对比：compact 借用 trim 水位 vs compact 有自己的水位。
#
#   A 组（改之前）：compact_at = trim_at = 0.9
#       两者同水位。裁剪跑在回合开头、压缩跑在回合末尾，裁剪永远先把上下文
#       压到线下，压缩等不到触发——上下文全靠删最老的轮维持。
#   B 组（改之后）：compact_at = 0.8，trim_at = 0.95
#       压缩先接手，裁剪退为兜底。
#
# 两组跑同一个二进制、同一串提示词，只差这两个数。看四件事：压缩触发几次、
# 裁剪触发几次、轮被删了多少、缓存命中率差多少。
#
# 窗口压到 32000 是为了让测试几分钟内跑到水位（真实 168000 的窗口要堆几十轮
# 才够得着），机制与真机一致。
#
#   bash testkit/cache-forensics/watermark_ab.sh run [轮数]
#   bash testkit/cache-forensics/watermark_ab.sh report
#   bash testkit/cache-forensics/watermark_ab.sh stop
set -euo pipefail

BIN=${MIYU_BIN:-$(cd "$(dirname "$0")/../.." && pwd)/target/debug/miyu}
MODEL=${MIYU_AB_MODEL:-opencodego/mimo-v2.6-flash}
REAL=$HOME/.miyu/config/config.jsonc
ROUNDS=${2:-14}
# 窗口不能压得太小。20000 那版两组都只剩 1 轮、一次压缩都没跑：系统提示词
# 加工具表就占 12700，可见历史最多 7300，全落在压缩的保尾预算（8192）里，
# `find_cut_index` 返回 0——压缩判定「没东西可折」直接 return，上下文全靠
# 裁剪删轮维持。窗口要留得下「保尾预算 + 一段够折的历史」。
WINDOW=60000

declare -A PORTS=([a]=8393 [b]=8394)
# arm -> "compact_at trim_at"
declare -A LEVELS=([a]="0.9 0.9" [b]="0.8 0.95")

home_for() { echo "$HOME/.cache/miyu-wm-ab-$1"; }

seed() {
  local arm=$1 dir levels
  dir=$(home_for "$arm")
  levels=${LEVELS[$arm]}
  mkdir -p "$dir/config"
  chmod 700 "$dir"
  python3 - "$REAL" "$dir/config/config.jsonc" "$WINDOW" ${levels} <<'PY'
import json, pathlib, re, sys

real, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
window, compact_at, trim_at = int(sys.argv[3]), float(sys.argv[4]), float(sys.argv[5])
cfg = json.loads(re.sub(r"^\s*//.*$", "", real.read_text(), flags=re.M))
carry = ["providers", "active_provider", "active_provider_models", "model_tiers", "prompt", "tools"]
seeded = {key: cfg[key] for key in carry if key in cfg}
seeded["config_version"] = cfg.get("config_version")
seeded["oobe_done"] = True
seeded["memory"] = {"enabled": False}
# 窗口的真相源是 provider 的 `model_context_window`，不是
# `context.default_context_window`（那个只是查不到元数据时的兜底）。第一版
# 只改了兜底，实测跑到 72k prompt 仍一次都没触发——mimo 配的窗口是
# 1,048,576，触发线在 83 万开外。这里把每个模型的窗口一起压小。
for provider in seeded.get("providers", []):
    windows = provider.get("model_context_window")
    if isinstance(windows, dict):
        provider["model_context_window"] = {name: window for name in windows}

context = dict(cfg.get("context", {}))
context.update(
    {
        "default_context_window": window,
        "compact_at_ratio": compact_at,
        "trim_at_ratio": trim_at,
        # 强制水位跟着压缩水位走，免得它反过来卡住 A 组。
        "compact_force_ratio": max(compact_at, 0.9),
        "trim_batch_ratio": 0.15,
        "on_overflow": "compact",
        # 关掉剪枝：它是两组的共同项，不影响「水位」这个自变量，但留着会
        # 把每轮增量压到 1/6，测试要跑几十轮才跨得过线。
        "tool_result_prune_chars": 0,
    }
)
seeded["context"] = context
out.write_text(json.dumps(seeded, ensure_ascii=False, indent=2), encoding="utf-8")
print(f"  [{out.parent.parent.name}] window={window} compact_at={compact_at} trim_at={trim_at}")
PY
  chmod 600 "$dir/config/config.jsonc"
}

start() {
  local arm=$1 dir
  dir=$(home_for "$arm")
  # 起不来时要看得见原因：第一版把 stderr 也吞了，只剩 set -e 让整个脚本
  # 悄悄退出，查了两轮才发现是上一次的 daemon 没停干净。
  if ! MIYU_HOME="$dir" "$BIN" daemon --port "${PORTS[$arm]}" start >/dev/null; then
    echo "  [$arm] daemon 起不来（home=$dir port=${PORTS[$arm]}）" >&2
    return 1
  fi
  echo "  [$arm] :${PORTS[$arm]}"
}

stop_all() {
  for arm in a b; do
    MIYU_HOME="$(home_for "$arm")" "$BIN" daemon stop >/dev/null 2>&1 || true
  done
  echo "两组 daemon 已停"
}

# 堆上下文用的负载：每轮一段中等大小的工具输出，够快够稳地把窗口填满。
prompt_for() {
  local index=$1
  echo "调用 run_command 执行这条命令（原样执行，不许加 wc/head/tail 或任何截断）：seq 1 1500 | paste -sd, -  。拿到完整输出后只回复「第 ${index} 轮完成」，不要复述输出。"
}

run() {
  echo "== 播配置 =="
  for arm in a b; do seed "$arm"; done
  echo "== 起 daemon =="
  for arm in a b; do start "$arm"; done
  sleep 3
  echo "== 交替跑 $ROUNDS 轮（A=改之前 / B=改之后） =="
  for i in $(seq 1 "$ROUNDS"); do
    for arm in a b; do
      MIYU_HOME="$(home_for "$arm")" timeout 600 "$BIN" --session wm --create \
        --model "$MODEL" "$(prompt_for "$i")" >/dev/null 2>&1 || echo "    [$arm] 第 $i 轮失败"
    done
    echo "    第 $i 轮完成（两组）"
  done
  report
}

report() {
  python3 "$(dirname "$0")/watermark_ab_report.py" "$(home_for a)" "$(home_for b)"
}

case "${1:-run}" in
  run) run ;;
  report) report ;;
  stop) stop_all ;;
  *) echo "用法: $0 [run [轮数]|report|stop]" >&2; exit 2 ;;
esac

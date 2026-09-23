#!/usr/bin/env bash
# 8388 沙盒:独立 MIYU_HOME,只从真配置里搬供应商与模型档位,不带账号、不接 QQ、
# 不挂 MCP/技能/插件——真模型能用,但碰不到你的会话、记忆与平台。
#
#   bash testkit/tui/sandbox8388.sh [start|stop|status]
set -euo pipefail
# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for __herdr_var in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$__herdr_var"; done

HOME_DIR=${MIYU_SANDBOX_HOME:-$HOME/.cache/miyu-sandbox-8388}
PORT=8388
BIN=${MIYU_BIN:-/home/shorin/Documents/github/Miyu/target/debug/miyu}
REAL=$HOME/.miyu/config/config.jsonc

seed_config() {
  mkdir -p "$HOME_DIR/config"
  chmod 700 "$HOME_DIR"
  python3 - "$REAL" "$HOME_DIR/config/config.jsonc" <<'PY'
import json, pathlib, re, sys

real, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
cfg = json.loads(re.sub(r"^\s*//.*$", "", real.read_text(), flags=re.M))

# 只搬「怎么调模型」这一层。账号/平台/MCP/技能/插件一律不带:沙盒要的是
# 真模型,不是真身份。
carry = [
    "providers",
    "active_provider",
    "active_provider_models",
    "active_multimodal_provider_models",
    "model_tiers",
    "display",
    "prompt",
    "context",
    "tools",
]
seeded = {key: cfg[key] for key in carry if key in cfg}
seeded["config_version"] = cfg.get("config_version")
seeded["oobe_done"] = True
# 记忆关掉:沙盒不该长出一份自己的长期记忆,也省掉建库那几秒。
seeded["memory"] = {"enabled": False}
out.write_text(json.dumps(seeded, ensure_ascii=False, indent=2), encoding="utf-8")
print(f"供应商 {len(seeded.get('providers', []))} 个 · 活跃 {seeded.get('active_provider')}")
PY
  chmod 600 "$HOME_DIR/config/config.jsonc"
}

case "${1:-start}" in
  start)
    seed_config
    MIYU_HOME="$HOME_DIR" "$BIN" daemon --port "$PORT" start
    ;;
  stop)
    MIYU_HOME="$HOME_DIR" "$BIN" daemon stop || true
    ;;
  status)
    ss -ltnp 2>/dev/null | grep ":$PORT" || echo "8388 没人听"
    ;;
  *)
    echo "用法: $0 [start|stop|status]" >&2
    exit 2
    ;;
esac

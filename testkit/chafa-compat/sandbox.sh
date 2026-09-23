#!/usr/bin/env bash
# 起一个专供「用不同终端手测图片渲染」的沙盒。
#
# 与 daemon 隔离的 MIYU_HOME、独立端口，沿用本机的供应商配置（QQ/语音关掉），
# 所以模型可用——直接在 REPL 里让她显示图片，走的就是用户真正会遇到的那条路。
#
# 用法：
#   testkit/chafa-compat/sandbox.sh            # 起沙盒
#   ~/.cache/miyu-chafa-sandbox/miyu-sb        # 在任意终端里开 REPL
#   ~/.cache/miyu-chafa-sandbox/with-chafa 1.14.5 ~/.cache/miyu-chafa-sandbox/miyu-sb
#                                              # 换用旧版 chafa 再测一遍
set -euo pipefail
# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for __herdr_var in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$__herdr_var"; done
ROOT=/home/shorin/.cache/miyu-chafa-sandbox
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$(cd "$HERE/../.." && pwd)/target/release/miyu"
SCRIPTS=/home/shorin/.local/share/miyu/scripts
PORT=8389

[ -x "$BIN" ] || { echo "二进制还没编译好: $BIN" >&2; exit 1; }

systemctl --user stop miyu-chafa-sandbox.service 2>/dev/null || true
rm -rf "$ROOT"
mkdir -p "$ROOT/home/config" "$ROOT/runtime" "$ROOT/images"

python3 - "$ROOT/home/config/config.jsonc" <<'PY'
import re, sys
src = open("/home/shorin/.miyu/config/config.jsonc", encoding="utf-8").read()
for key in ("qq", "voice"):
    src, n = re.subn(r'("%s":\s*\{\s*\n\s*"enabled":\s*)true' % key, r'\1false', src, count=1)
    assert n == 1, key
open(sys.argv[1], "w", encoding="utf-8").write(src)
PY
chmod 600 "$ROOT/home/config/config.jsonc"

# 测试图：一张宽的、一张高的、一张小表情包尺寸的，外加一张 GIF 首帧场景。
python3 - "$ROOT/images" <<'PY'
import struct, sys, zlib, os
out = sys.argv[1]
def png(path, w, h):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
    raw = b""
    for y in range(h):
        row = b""
        for x in range(w):
            # 细网格 + 渐变：缩放、抖动、调色板的毛病一眼能看出来
            grid = 255 if (x % 16 == 0 or y % 16 == 0) else 0
            row += bytes([max(grid, (x * 255) // w), max(grid, (y * 255) // h), grid])
        raw += b"\x00" + row
    data = (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw))
            + chunk(b"IEND", b""))
    open(os.path.join(out, path), "wb").write(data)
png("wide.png", 640, 240)
png("tall.png", 240, 640)
png("small.png", 128, 128)
png("big.png", 1024, 768)
PY

systemd-run --user --unit=miyu-chafa-sandbox --collect -E LANG=zh_CN.UTF-8 -E LANGUAGE=zh_CN:en \
  -p WorkingDirectory="$ROOT/home" \
  --setenv=MIYU_HOME="$ROOT/home" --setenv=XDG_RUNTIME_DIR="$ROOT/runtime" \
  --setenv=MIYU_SYSTEM_SCRIPTS_DIR="$SCRIPTS" \
  "$BIN" __daemon --port "$PORT"

for _ in $(seq 1 60); do
  curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/health" && break
  sleep 0.5
done

# REPL 入口。MIYU_IMAGE_TRACE 打开，每次打图都往
# ~/.miyu/cache/logs/image-trace.log 追一行（chafa 版本、参数、选中格式、耗时）。
cat > "$ROOT/miyu-sb" <<EOF
#!/usr/bin/env bash
export MIYU_HOME=$ROOT/home
export XDG_RUNTIME_DIR=$ROOT/runtime
export MIYU_SYSTEM_SCRIPTS_DIR=$SCRIPTS
export MIYU_IMAGE_TRACE=1
exec $BIN "\$@"
EOF
chmod +x "$ROOT/miyu-sb"

# 换用历史版本的 chafa 跑同一条命令：with-chafa 1.14.5 <命令...>
OLD="$HERE/old"
cat > "$ROOT/with-chafa" <<EOF
#!/usr/bin/env bash
# 把 PATH 前置到指定版本的 chafa 上，验证旧发行版的表现。
set -euo pipefail
ver="\$1"; shift
dir=$OLD/v\$ver
[ -x "\$dir/usr/bin/chafa" ] || { echo "没有 \$ver，先跑 testkit/chafa-compat/fetch-old-chafa.sh" >&2; exit 1; }
shim=\$(mktemp -d)
cat > "\$shim/chafa" <<SHIM
#!/bin/sh
exec env LD_LIBRARY_PATH=$OLD/v\$ver/usr/lib:$OLD/jxl/usr/lib $OLD/v\$ver/usr/bin/chafa "\\\$@"
SHIM
chmod +x "\$shim/chafa"
PATH="\$shim:\$PATH" "\$@"
EOF
chmod +x "$ROOT/with-chafa"

: > /home/shorin/.miyu/cache/logs/image-trace.log 2>/dev/null || true

echo "沙盒就绪 (端口 $PORT)"
echo "  二进制   $BIN"
echo "  测试图   $ROOT/images/{small,wide,tall,big}.png"
echo "  REPL     $ROOT/miyu-sb"
echo "  旧 chafa $ROOT/with-chafa 1.14.5 $ROOT/miyu-sb"
echo "  取证日志 ~/.miyu/cache/logs/image-trace.log"
echo "  health=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/api/health")"
echo "  本机 chafa $(chafa --version 2>/dev/null | head -1)"

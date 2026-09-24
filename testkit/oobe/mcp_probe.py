#!/usr/bin/env python3
"""引导页「MCP 服务器」一格的无头探针(09-16):真二进制 + pyte,不碰真配置。

用法: python3 mcp_probe.py [binary] [cols] [rows]

临时 MIYU_HOME 里先 `miyu init`,往 config.jsonc 塞两台 MCP 服务器(一台没显示名),
再进 `miyu oobe`:欢迎 → 人格(自己捏、起名)→ 功能。断言:
  1. 功能屏出现「MCP 服务器」分组(排最后,先 End 滚到底)、两台服务器(显示名 / 退回 id)、默认都勾着;
  2. Ctrl+A 按到全关(自定义人格下内置件默认不勾,第一下是全开),MCP 两项都变 [ ];
  3. Enter 离开功能屏(写 persona.toml)后,清单里 `mcp = []`(明细,不是 None)。
最后 kill 掉引导进程,打印结论;任一条不过退出码 1。
"""
import fcntl, json, os, pty, select, struct, subprocess, sys, tempfile, termios, time

import pyte
import os
import sys
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "debug", "miyu"
)
COLS = int(sys.argv[2]) if len(sys.argv) > 2 else 100
ROWS = int(sys.argv[3]) if len(sys.argv) > 3 else 60

home = str(sandbox_dir.make("miyu-oobe-mcp-"))
env = dict(os.environ)
env.update({"TERM": "xterm-256color", "MIYU_HOME": home, "LANG": "zh_CN.UTF-8"})
subprocess.run([BIN, "init"], env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
cfg_path = os.path.join(home, "config", "config.jsonc")
cfg = json.loads(open(cfg_path, encoding="utf-8").read())
cfg["mcp"] = {
    "enabled": True,
    "servers": [
        {"id": "probe_named", "display_name": "探针服务器甲", "command": "echo", "args": ["mcp-a"], "enabled": True},
        {"id": "probe_bare", "command": "echo", "args": ["mcp-b"], "enabled": True},
        {"id": "probe_off", "display_name": "关着的不该出现", "command": "echo", "enabled": False},
    ],
}
cfg["oobe_done"] = False
open(cfg_path, "w", encoding="utf-8").write(json.dumps(cfg, ensure_ascii=False, indent=2))

screen = pyte.Screen(COLS, ROWS)
stream = pyte.ByteStream(screen)
pid, fd = pty.fork()
if pid == 0:
    os.environ.update({
        "TERM": "xterm-256color",
        "MIYU_HOME": home,
        "MIYU_OOBE_NO_IME": "1",
        "MIYU_OOBE_VERBOSE": "1",
        "LANG": "zh_CN.UTF-8",
    })
    os.execvp(BIN, [BIN, "oobe"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))


def pump(t=0.3):
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            stream.feed(data)
            for _ in range(data.count(b"\x1b[6n")):
                os.write(fd, f"\x1b[{screen.cursor.y + 1};{screen.cursor.x + 1}R".encode())


def send(keys, t=0.35):
    os.write(fd, keys)
    pump(t)


def display():
    rows = []
    for y in range(ROWS):
        line = screen.buffer[y]
        out = []
        x = 0
        while x < COLS:
            data = line[x].data if x in line else " "
            if data == "":
                x += 1
                continue
            out.append(data)
            x += 1
        rows.append("".join(out))
    return rows


def shot(title):
    print()
    print(f"┌── {title} " + "─" * max(0, COLS - len(title) - 6))
    for line in display():
        print("│" + line.rstrip())
    print("└" + "─" * (COLS - 1))


ENTER, TAB, SPACE, DOWN, END = b"\r", b"\t", b" ", b"\x1b[B", b"\x1b[F"
CTRL_A, CTRL_S = b"\x01", b"\x13"

pump(2.5)
send(SPACE, 0.6)
send(ENTER, 0.6)
send(DOWN, 0.3)
send(DOWN, 0.3)
send(ENTER, 0.3)
send("小满".encode(), 0.3)
send(ENTER, 0.3)
send(DOWN, 0.3)
send(ENTER, 0.9)
# MCP 排在最后一组,矮终端下会被滚动视口裁掉(40 行就看不见,表头计数仍算它);
# 先 End 把光标挪到最后一项,视口跟着滚过去再抓帧。
send(END, 0.4)
shot("02 功能(默认全勾,光标在最后一项)")
rows = display()
checks = []
checks.append(("功能屏有「MCP 服务器」分组", any("MCP 服务器" in l for l in rows)))
body = "\n".join(rows)
checks.append(("有显示名的服务器按显示名列出", "探针服务器甲" in body))
checks.append(("没显示名的退回 id", "probe_bare" in body))
checks.append(("enabled=false 的不出现", "关着的不该出现" not in body and "probe_off" not in body))
named_line = next((l for l in rows if "探针服务器甲" in l and "[" in l), "")
bare_line = next((l for l in rows if "probe_bare" in l and "[" in l), "")
checks.append(("两台默认都勾着", "[*]" in named_line and "[*]" in bare_line))

# 自定义人格下内置脚本/技能默认不勾,Ctrl+A 第一下是「全开」,第二下才是「全关」;
# 按到 MCP 两项都变 [ ] 为止(最多两下)。
for _ in range(2):
    send(CTRL_A, 0.6)
    rows = display()
    if all("[ ]" in l for l in rows if ("探针服务器甲" in l or "probe_bare" in l) and "[" in l):
        break
shot("02 功能(Ctrl+A 到全关)")
rows = display()
named_line = next((l for l in rows if "探针服务器甲" in l and "[" in l), "")
bare_line = next((l for l in rows if "probe_bare" in l and "[" in l), "")
checks.append(("Ctrl+A 后两台都变 [ ]", "[ ]" in named_line and "[ ]" in bare_line and "[*]" not in named_line))

send(ENTER, 0.8)  # 离开功能屏 → 写 persona.toml
shot("03 认识你(功能已落盘)")
manifest_text = ""
for root, _, files in os.walk(home):
    for name in files:
        if name == "persona.toml":
            manifest_text = open(os.path.join(root, name), encoding="utf-8").read()
            print("── persona.toml:", os.path.relpath(os.path.join(root, name), home))
            print(manifest_text)
checks.append(("persona.toml 写了 mcp = [](明细而非 None)", "mcp=[]" in manifest_text.replace(" ", "")))

try:
    os.kill(pid, 15)
except ProcessLookupError:
    pass
print("家目录:", home, "（跑完即删；要留着看设 MIYU_KEEP_SANDBOX=1）")
try:
    after = json.loads(open(cfg_path, encoding="utf-8").read())
    print("跑完后 config.jsonc 的 mcp 段:", json.dumps(after.get("mcp"), ensure_ascii=False))
except Exception as error:  # noqa: BLE001
    print("跑完后读 config.jsonc 失败:", error)

print()
failed = 0
for label, ok in checks:
    print(("PASS " if ok else "FAIL ") + label)
    failed += 0 if ok else 1
print(f"{len(checks) - failed}/{len(checks)} passed")
sys.exit(1 if failed else 0)

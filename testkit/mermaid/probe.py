#!/usr/bin/env python3
"""不开真终端，只看 miyu 往 tty 上吐了什么字节。

用来把「图没画出来」拆成两截：是渲染器没出图（字节流里没有 `ESC_G` 传输段），
还是出了图但 kitty 没画（字节里有、屏幕上没有）。真 kitty 那条走查（terminal.py）
只能回答后者，这条回答前者。

跑法：python3 testkit/mermaid/probe.py [mermaid|rust]
"""

import json
import os
import pty
import re
import shutil
import socket
import subprocess
import sys
import time
import urllib.parse
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

HOME = Path("/tmp/miyu-mermaid-probe/home")
REPO = Path(__file__).resolve().parents[2]
BIN = REPO / "target" / "debug" / "miyu"
SMOKE = REPO / "testkit" / "repl-smoke"

FLOW = (
    "flowchart TD\n"
    "    A[用户提问] --> B{要用工具吗}\n"
    "    B -->|要| C[调用工具]\n"
    "    B -->|不要| D[直接回答]\n"
    "    C --> D"
)
BODIES = {"mermaid": ("mermaid", FLOW), "rust": ("rust", 'fn main() {\n    println!("hi");\n}')}


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def write_config(port):
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(
            {
                "config_version": 3,
                "oobe_done": True,
                "active_provider": "stub",
                "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
                "providers": [
                    {
                        "id": "stub",
                        "display_name": "stub",
                        "base_url": f"http://127.0.0.1:{port}/v1",
                        "api_key": "sk-test",
                        "models": ["stub-model"],
                        "default_model": "stub-model",
                    }
                ],
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )


def main():
    which = sys.argv[1] if len(sys.argv) > 1 else "mermaid"
    lang, body = BODIES[which]
    if HOME.exists():
        shutil.rmtree(HOME)
    port = free_port()
    write_config(port)
    reply = f"看这张图：\n\n```{lang}\n{body}\n```\n"
    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(port), STUB_REPLY=reply),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(80):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            break
        except OSError:
            time.sleep(0.1)

    chunks = []
    try:
        # 真 pty：isatty 为真、TERM 是 kitty，走的就是终端那条渲染路。
        pid, fd = pty.fork()
        if pid == 0:
            os.environ.update(
                MIYU_HOME=str(HOME),
                MIYU_DIRECT="1",
                MIYU_TUI="0",
                TERM="xterm-kitty",
                COLUMNS="100",
                LINES="40",
            )
            os.execv(str(BIN), [str(BIN), "画个图"])
        deadline = time.time() + 180
        while time.time() < deadline:
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            chunks.append(data)
        os.waitpid(pid, 0)
    finally:
        stub.terminate()
        try:
            stub.wait(timeout=5)
        except subprocess.TimeoutExpired:
            stub.kill()

    raw = b"".join(chunks)
    Path(f"/tmp/miyu-mermaid-probe/{which}.raw").write_bytes(raw)
    apc = re.findall(rb"\x1b_G([^\x1b]*)\x1b\\", raw)
    print(f"{which}: {len(raw)} 字节, kitty 图片段 {len(apc)} 个")
    if apc:
        print(f"  首段控制字段：{apc[0][:120].decode('ascii', 'replace')}")

    report = {}
    if which == "mermaid":
        report["出了图形传输段"] = bool(apc)
        report["走 PNG 传输（f=100，不是裸 RGBA）"] = bool(apc) and b"f=100" in apc[0]
        # 「点开看大图」那行：OSC 8 链接指向缓存里的 SVG。
        link = re.search(rb"\x1b\]8;;(file://[^\x1b\x07]+)\x1b\\(.*?)\x1b\]8;;", raw, re.S)
        report["图下面有 OSC 8 链接"] = link is not None
        if link:
            url = link.group(1).decode()
            label = re.sub(rb"\x1b\[[0-9;]*m", b"", link.group(2)).decode("utf-8", "replace")
            target = Path(urllib.parse.unquote(url[len("file://"):]))
            print(f"  链接 → {target}\n  标签 → {label!r}")
            report["链接指向真存在的文件"] = target.is_file()
            # 指 SVG 不指 PNG：放大只有矢量做得好。代价是 `image/svg+xml` 的
            # 默认程序得自己指一次（`xdg-mime default <看图器> image/svg+xml`），
            # 否则会落到系统级默认（这台机器原来是 Curtail，一个图片压缩工具）。
            report["指的是 SVG（矢量才放得大）"] = target.suffix == ".svg"
            report["文件真是一张 SVG"] = target.is_file() and target.read_bytes()[:4] == b"<svg"
            report["标签是「点开看大图」"] = label.strip() == "点开看大图"
    else:
        report["rust 围栏没出图"] = not apc

    text = re.sub(rb"\x1b_G[^\x1b]*\x1b\\", b"", raw)
    text = re.sub(rb"\x1b\[[0-9;?]*[a-zA-Z]", b"", text)
    print("  可见文字：")
    print("   " + text.decode("utf-8", "replace").replace("\r\n", "\n").replace("\n", "\n   ")[:800])

    passed = sum(1 for ok in report.values() if ok)
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())

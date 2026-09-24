#!/usr/bin/env python3
"""429 这类报错要说人话（BUG-16）。

起一个只会回 429 的假供应商，配成两个端点的池，让 TUI 真跑一轮撞上去，看屏上
那段报错长什么样：以前是一行英文 + 原始 JSON（`upstream returned HTTP 429` 把
分类结论整个丢了），现在该是「一句结论 + 每个端点一行」。末尾原来还有一句
「→ 等冷却结束，或换一个供应商」，09-20 用户点名去掉了（c3474af5），这里反过来验它不在。

跑法：

    cargo build
    python3 testkit/tui/error_visual.py

产物在 ~/.cache/miyu-error-visual/（screen.txt 就是给人看的那张图）。

**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和端口。
"""

import json
import os
import shutil
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-error-visual"))
PROMPT = "走查一句"
QUOTA_PORT = int(os.environ.get("QUOTA_PORT", "18497"))
# 真实的 OpenAI 系限流报文，够长，正好也验「每条理由按条裁」那一段。
BODY = json.dumps(
    {
        "error": {
            "message": (
                "Rate limit reached for model in organization org-miyu on tokens per min "
                "(TPM): Limit 30000, Used 30000, Requested 512. Please try again in 1.024s. "
                "Visit https://platform.openai.com/account/rate-limits to learn more."
            ),
            "type": "tokens",
            "code": "rate_limit_exceeded",
        }
    },
    ensure_ascii=False,
)


class Quota429(BaseHTTPRequestHandler):
    """一个只会说「你没额度了」的供应商。"""

    def do_POST(self):
        self.send_response(429)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(BODY.encode())

    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"data":[]}')

    def log_message(self, *args):
        pass


def write_config():
    (h.HOME / "config").mkdir(parents=True, exist_ok=True)
    provider = {
        "display_name": "没额度了",
        "base_url": f"http://127.0.0.1:{QUOTA_PORT}/v1",
        "protocol": "openai-chat",
        "api_key": "stub",
        "models": ["broke-model"],
    }
    config = {
        "active_provider": "brokeA",
        # 两个端点的池：报错要一条一条列出来，看得见「试过谁」。
        "active_provider_models": [
            {"provider_id": "brokeA", "model": "broke-model"},
            {"provider_id": "brokeB", "model": "broke-model"},
        ],
        "providers": [
            dict(provider, id="brokeA"),
            dict(provider, id="brokeB"),
        ],
        "memory": {"enabled": False},
    }
    (h.HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def main():
    if not h.BIN.exists():
        print(f"! 先 cargo build：{h.BIN} 不存在", file=sys.stderr)
        return 2
    if h.HOME.exists():
        shutil.rmtree(h.HOME)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    write_config()
    h.kill_stale_daemon()

    server = ThreadingHTTPServer(("127.0.0.1", QUOTA_PORT), Quota429)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    daemon = tui = None
    report = {}
    try:
        daemon = subprocess.Popen(
            [str(h.BIN), "__daemon", "--port", str(h.PORT)],
            env=h.ENV, cwd=str(h.HOME),
            stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT,
        )
        if not h.wait_http(f"{h.BASE}/api/config", timeout=30):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        tui, master = h.spawn_tui()
        sink = bytearray()
        h.drain(master, 3.0, sink)
        os.write(master, PROMPT.encode())
        h.drain_until(master, sink, PROMPT, 3.0)
        os.write(master, b"\r")
        h.drain_until(master, sink, "错误", 60.0)
        h.drain(master, 2.0, sink)
        screen = h.render(bytes(sink))
        (OUT / "screen.txt").write_text("\n".join(screen), encoding="utf-8")
        text = "\n".join(screen)

        report["说清了是限流/额度"] = "被限流或额度用完" in text
        report["两个端点都列出来了"] = "brokeA" in text and "brokeB" in text
        report["说了冷却多久"] = "暂停 10 分钟" in text
        # 报错那一段：从「错误」那行起，到空行或输入框的竖条为止。
        start = next((i for i, line in enumerate(screen) if "错误" in line), None)
        block = []
        for line in screen[start:] if start is not None else []:
            if not line.strip() or line.lstrip().startswith(h.BAR):
                break
            block.append(line)
        report["不再多一句该怎么办"] = bool(block) and not any(
            line.lstrip().startswith("→") or "换一个供应商" in line for line in block
        )
        report["留了请求 id 备查"] = "llm_" in text
        report["留了状态码备查"] = "429" in text
        report["不再是英文模板"] = "no LLM provider/model endpoint succeeded" not in text
        report["不再直接倒原始 JSON"] = '"rate_limit_exceeded"' not in text
    finally:
        server.shutdown()
        for process in (tui, daemon):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    passed = 0
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())

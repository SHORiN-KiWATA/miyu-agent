#!/usr/bin/env python3
"""开发模式提示词默认为空(09-24)的黑盒:沙箱 home + 独立端口 daemon + 记系统提示词的桩。

    BIN=<miyu> python3 testkit/dev-prompt/run.py

判定:
  新家目录初始化后没有 dev-prompt.md
  没写提示词:开发模式发出去的 system 从 <host-environment 开始,没有内置角色句
  文件里恰好是老版本自动写进去的那句:照样当没写
  写了自己的:原样放最前面,和环境块之间空一行
"""
import json
import os
import shutil
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BIN = Path(os.environ["BIN"])
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-dev-prompt")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18551"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18552"))
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), LANG="zh_CN.UTF-8")
for key in ("MIYU_DIRECT", "MIYU_SESSION", "MIYU_TURN_MODE", "XDG_CACHE_HOME", "XDG_CONFIG_HOME",
            "XDG_DATA_HOME", "XDG_STATE_HOME"):
    ENV.pop(key, None)
LEGACY = "You are a helpful software engineer assistant."
SYSTEMS = []
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}" + (f"  ({detail})" if detail and not ok else ""))


class Stub(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))) or b"{}")
        for message in body.get("messages", []):
            if message.get("role") == "system":
                content = message.get("content")
                if isinstance(content, list):
                    content = "".join(p.get("text", "") for p in content if isinstance(p, dict))
                SYSTEMS.append(content or "")
                break
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for delta, finish in (({"role": "assistant", "content": "好"}, None), ({}, "stop")):
            chunk = {"id": "stub", "object": "chat.completion.chunk", "model": "stub-a",
                     "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            self.wfile.write(f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def do_GET(self):
        payload = json.dumps({"data": [{"id": "stub-a"}]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-a"}],
        "providers": [{
            "id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
            "protocol": "openai-chat", "api_key": "stub", "models": ["stub-a"],
        }],
        "memory": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False, indent=2), "utf-8")


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


def ask_dev(text):
    before = len(SYSTEMS)
    subprocess.run([str(BIN), "ask", "--output-format", "json", "--mode", "dev", text],
                   env=ENV, cwd=str(HOME), stdin=subprocess.DEVNULL, capture_output=True, timeout=90)
    return SYSTEMS[before] if len(SYSTEMS) > before else None


def dev_prompt_file():
    found = list(HOME.rglob("dev-prompt.md"))
    return found[0] if found else HOME / "config" / "dev-prompt.md"


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True)
    write_config()
    server = ThreadingHTTPServer(("127.0.0.1", STUB_PORT), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    daemon = None
    try:
        daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                                  stdin=subprocess.DEVNULL, stdout=(OUT / "daemon.log").open("w"),
                                  stderr=subprocess.STDOUT)
        assert wait_http(f"http://127.0.0.1:{PORT}/"), "daemon 没起来(看 daemon.log)"
        time.sleep(1.0)
        check("新家目录初始化后没有 dev-prompt.md", not list(HOME.rglob("dev-prompt.md")),
              str(list(HOME.rglob("dev-prompt.md"))))

        system = ask_dev("你好")
        check("没写提示词:从环境块开始、没有角色句",
              system is not None and system.startswith("<host-environment") and LEGACY not in system,
              repr((system or "")[:80]))

        path = dev_prompt_file()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(LEGACY + "\n", "utf-8")
        system = ask_dev("再来")
        check("文件里是老默认那句:照样当没写",
              system is not None and system.startswith("<host-environment") and LEGACY not in system,
              repr((system or "")[:80]))

        path.write_text("你是资深前端工程师\n", "utf-8")
        system = ask_dev("第三句")
        check("写了自己的:原样放最前、和环境块之间空一行",
              system is not None and system.startswith("你是资深前端工程师\n\n<host-environment"),
              repr((system or "")[:80]))
    finally:
        if daemon:
            daemon.terminate()
            try:
                daemon.wait(5)
            except Exception:
                daemon.kill()
        server.shutdown()
    print(f"{sum(results)}/{len(results)} passed")
    sys.exit(0 if results and all(results) else 1)


if __name__ == "__main__":
    main()

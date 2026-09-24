"""网页走查的沙箱(09-24):隔离家目录 + 独立端口 daemon + 桩 LLM + 登录。

`testkit/webui-composer/walk.py` 与 `testkit/turn-resume/webui.py` 共用。桩模型是
`testkit/turn-resume/stub.py`(懂 `TK slow <记号> [秒]` 与续跑消息)。全在 /tmp 下,
不碰线上 8300。
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "webui-fixes"))
import authlib  # noqa: E402

STUB = HERE.parent / "turn-resume" / "stub.py"


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


class WebSandbox:
    def __init__(self, binary: Path, out: Path, extra_providers=None):
        self.binary = binary
        self.extra_providers = list(extra_providers or [])
        self.out = out
        self.home = out / "home"
        self.runtime = out / "run"
        self.port = free_port()
        self.stub_port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.stub = None
        self.daemon = None
        self.boots = 0

    def env(self):
        env = {k: v for k, v in os.environ.items()
               if not k.startswith("HERDR_")
               and k not in ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "MIYU_HOME")}
        env.update(MIYU_HOME=str(self.home), XDG_RUNTIME_DIR=str(self.runtime), LANG="zh_CN.UTF-8")
        return env

    def start(self):
        if self.out.exists():
            shutil.rmtree(self.out)
        self.runtime.mkdir(parents=True)
        (self.home / "config").mkdir(parents=True)
        config = {
            "config_version": 3,
            "oobe_done": True,
            "active_provider": "stub",
            "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
            "providers": [{
                "id": "stub", "display_name": "Stub", "enabled": True,
                "base_url": f"http://127.0.0.1:{self.stub_port}/v1", "protocol": "openai-chat",
                "api_key": "stub-key", "models": ["stub-model"], "default_model": "stub-model",
            }, *self.extra_providers],
            "memory": {"enabled": False},
            "voice": {"enabled": False},
        }
        (self.home / "config" / "config.jsonc").write_text(json.dumps(config), "utf-8")
        stub_env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        stub_env.update(STUB_PORT=str(self.stub_port), STUB_LOG=str(self.out / "stub.jsonl"))
        self.stub = subprocess.Popen([sys.executable, str(STUB)], env=stub_env,
                                     stdout=subprocess.DEVNULL, stderr=(self.out / "stub.err").open("w"))
        assert wait_http(f"http://127.0.0.1:{self.stub_port}/v1/models"), "stub not up"
        self.start_daemon()
        authlib.bootstrap(self.base)

    def start_daemon(self):
        self.boots += 1
        log = (self.out / f"daemon-{self.boots}.log").open("w")
        self.daemon = subprocess.Popen([str(self.binary), "__daemon", "--port", str(self.port)],
                                       env=self.env(), cwd=str(self.home), stdin=subprocess.DEVNULL,
                                       stdout=log, stderr=subprocess.STDOUT)
        assert wait_http(f"{self.base}/api/health"), f"daemon #{self.boots} not up"

    def kill_daemon(self, sig=signal.SIGKILL):
        self.daemon.send_signal(sig)
        self.daemon.wait(timeout=30)

    def stop(self):
        for proc in (self.daemon, self.stub):
            if proc and proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    proc.kill()

    def api(self, method, path, payload=None):
        data = json.dumps(payload).encode() if payload is not None else None
        req = urllib.request.Request(self.base + path, data=data, method=method,
                                     headers={"content-type": "application/json"})
        with authlib.OPENER.open(req, timeout=15) as resp:
            raw = resp.read()
            return json.loads(raw) if raw else {}

    def create_session(self, name):
        return self.api("POST", "/api/sessions", {"name": name})["session"]["session_id"]

    def turns(self, session_id):
        return self.api("GET", f"/api/sessions/{session_id}/turns").get("turns", [])

    def sessions(self):
        return self.api("GET", "/api/sessions").get("sessions", [])

    def open_page(self, playwright, viewport=None):
        browser = playwright.chromium.launch()
        page = browser.new_page(viewport=viewport or {"width": 1280, "height": 900}, locale="zh-CN")
        page.on("dialog", lambda dialog: dialog.accept())
        page.goto(self.base)
        authlib.ui_login(page)
        page.wait_for_selector("#composerInput:not([disabled])", timeout=20000)
        return browser, page

    def view(self, page, session_id):
        page.click(f'.session-item[data-session-id="{session_id}"] .session-item-main')
        page.wait_for_timeout(1500)

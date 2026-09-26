#!/usr/bin/env python3
"""网页回复末尾那行 ✻ 与混合模型池的「供应商 / 模型」（09-26）。

    BIN=<miyu> python3 testkit/webui-turn-end/run.py

沙箱 daemon + OpenAI 桩（MODE=plain）+ Playwright（无头 Chromium）。会话池两个模型，所以网页原来在
用量那一行开头写「stub / stub-model」，回复末尾那行 ✻ 又写一遍模型名（用户 09-26：写了两遍）：
- 跑完那一段回复末尾有一行 `✻ stub / stub-… · <动词> N 秒 · H:MM 完成`；
- 用量那一行的「供应商 / 模型」藏起来了，模型名只出现一次；
- 刷新之后还是这样（按落库的回合重画）。
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from playwright.sync_api import sync_playwright

for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

KIT = Path(__file__).resolve().parent.parent / "webui-fixes"
sys.path.insert(0, str(KIT))
import authlib  # noqa: E402

BIN = Path(os.environ["BIN"])
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-webui-turn-end")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18597"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18598"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME))
END_RE = re.compile(r"^✻ stub / stub-\S+ · \S+ ?(?:\d+ 秒|不到 1 秒|\d+ 分 \d+ 秒) · .+ 完成$")
RESULTS = []


def check(label, ok, detail=""):
    RESULTS.append(ok)
    print(f"{'✅' if ok else '❌'} {label}" + (f"  ({detail})" if detail else ""))


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [
            {"provider_id": "stub", "model": "stub-model"},
            {"provider_id": "stub", "model": "stub-b"},
        ],
        "providers": [{"id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
                       "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model", "stub-b"]}],
        "memory": {"enabled": False},
        "display": {"language": "zh"},
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


def api(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers={"content-type": "application/json", "Origin": BASE})
    with authlib.OPENER.open(req, timeout=30) as resp:
        raw = resp.read()
    return json.loads(raw) if raw else None


def completed(session_id):
    turns = api("GET", f"/api/sessions/{session_id}/turns").get("turns") or []
    return [turn for turn in turns if turn.get("status") == "completed"]


def wait_until(check_fn, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if check_fn():
            return True
        time.sleep(0.3)
    return False


LAST_REPLY = """() => {
  const article = [...document.querySelectorAll('.assistant-message')].pop();
  if (!article) return null;
  const end = article.querySelector(':scope > .turn-end');
  const endpoint = article.querySelector('.assistant-meta .assistant-endpoint');
  return {
    end: end ? end.textContent : '',
    endpointVisible: !!endpoint && !endpoint.hidden && endpoint.offsetParent !== null,
    text: article.innerText,
  };
}"""


def inspect(page, tag):
    reply = page.evaluate(LAST_REPLY) or {}
    end = reply.get("end") or ""
    check(f"{tag}：回复末尾那行 ✻ 写着「供应商 / 模型」", bool(END_RE.match(end)), end)
    check(f"{tag}：用量那一行的「供应商 / 模型」藏起来了", not reply.get("endpointVisible"))
    text = reply.get("text") or ""
    names = re.findall(r"stub-(?:model|b)\b", text)
    check(f"{tag}：模型名只出现一次", len(names) == 1, f"{len(names)} 次")


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    stub_env = dict(os.environ, STUB_PORT=str(STUB_PORT), MODE="plain", STUB_CHUNK_SLEEP="0")
    stub = subprocess.Popen([sys.executable, str(KIT / "stub_reasoning.py")], env=stub_env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    daemon = None
    try:
        assert wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"), "stub not up"
        daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                                  stdin=subprocess.DEVNULL,
                                  stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
        assert wait_http(f"{BASE}/api/health"), "daemon not up"
        authlib.bootstrap(BASE)
        created = api("POST", "/api/sessions", {"name": "收尾行", "switch": True})
        session_id = (created.get("session") or {}).get("session_id")
        assert session_id, created
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page(viewport={"width": 1280, "height": 860})
            page.goto(BASE)
            authlib.ui_login(page)
            page.wait_for_selector("#composerInput", timeout=20000)
            page.fill("#composerInput", "走查一句")
            page.keyboard.press("Enter")
            assert wait_until(lambda: len(completed(session_id)) == 1, 60), "那一轮没跑完"
            page.wait_for_timeout(1500)
            page.screenshot(path=str(OUT / "live.png"))
            inspect(page, "跑完当场")
            page.reload()
            page.wait_for_selector("#timeline .user-message", timeout=20000)
            page.wait_for_timeout(1500)
            page.screenshot(path=str(OUT / "reloaded.png"))
            inspect(page, "刷新之后")
            browser.close()
    finally:
        if daemon:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
        stub.terminate()
    passed = sum(1 for ok in RESULTS if ok)
    print(f"{passed}/{len(RESULTS)} passed  产物：{OUT}")
    return 0 if RESULTS and passed == len(RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())

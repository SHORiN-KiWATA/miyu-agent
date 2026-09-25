#!/usr/bin/env python3
"""B12(09-24):运行中在设置页新加的模型,要查得到它的元数据。

    BIN=<miyu> python3 testkit/model-meta/run.py

沙箱 daemon(隔离 MIYU_HOME,默认端口 18561)。配置里只有 deepseek/deepseek-v4-flash。
启动后等 models.dev 那次联网刷新落地——它按当时的配置裁剪目录——再经
PUT /api/config 把 deepseek-v4-pro 加进来并设成当前模型,查当前会话的上下文:
窗口要有出处(context_window_assumed=false),数值等于目录里写的。修前是
assumed=true:目录里没有它,退回了通用默认值。

要能连上 models.dev(联网刷新落地是 bug 的前提)。供应商 base_url 指向本机空端口,
不会真的调用 DeepSeek。WebUI 要登录:没建管理员前,内置账号 miyu/miyu 登录即管理员。
"""
import http.client
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里:HERDR_* 漏给被测的 miyu 会搅乱那个 pane(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BIN = Path(os.environ["BIN"])
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-model-meta")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18561"))
DEAD_PORT = int(os.environ.get("DEAD_PORT", "18562"))
BASE = f"http://127.0.0.1:{PORT}"
ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), MIYU_LOG="info")
PROVIDER, FIRST, ADDED = "deepseek", "deepseek-v4-flash", "deepseek-v4-pro"

RESULTS = []


def check(label, ok, detail=""):
    RESULTS.append(ok)
    print(f"{'✅' if ok else '❌'} {label}" + (f"  ({detail})" if detail else ""))


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": PROVIDER,
        "active_provider_models": [{"provider_id": PROVIDER, "model": FIRST}],
        "providers": [{
            "id": PROVIDER, "display_name": "DeepSeek",
            "base_url": f"http://127.0.0.1:{DEAD_PORT}/v1",
            "protocol": "openai-chat", "api_key": "stub",
            "models": [FIRST], "default_model": FIRST,
        }],
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


COOKIE = []


def api(method, path, body=None):
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    headers = {"Accept": "application/json", "Origin": BASE}
    if COOKIE:
        headers["Cookie"] = COOKIE[0]
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    conn.request(method, path, body=data, headers=headers)
    response = conn.getresponse()
    set_cookie = response.getheader("Set-Cookie")
    if set_cookie:
        COOKIE[:] = [set_cookie.split(";", 1)[0]]
    raw = response.read()
    conn.close()
    try:
        return response.status, (json.loads(raw) if raw else None)
    except json.JSONDecodeError:
        return response.status, raw.decode(errors="replace")


def catalogue_window(catalogue, model):
    """与 models_cache::api 的 usable_context 同一口径:有输入上限就取两者较小。"""
    limit = catalogue.get(PROVIDER, {}).get("models", {}).get(model, {}).get("limit") or {}
    context, cap = limit.get("context") or 0, limit.get("input") or 0
    if context and cap:
        return min(context, cap)
    return context or cap or None


def current_context():
    status, sessions = api("GET", "/api/sessions")
    if status != 200:
        return status, sessions
    return api("GET", f"/api/sessions/{sessions['current']}/context")


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    write_config()
    cache_file = HOME / "cache" / "models_cache.json"
    daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)], env=ENV, cwd=str(HOME),
                              stdin=subprocess.DEVNULL,
                              stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
    try:
        assert wait_http(f"{BASE}/"), "daemon not up"
        deadline = time.time() + 90
        while time.time() < deadline and not cache_file.exists():
            time.sleep(0.5)
        if not cache_file.exists():
            print("⏭  models.dev 连不上,联网刷新没落地,这条测不了")
            return 2
        # 目录先落盘、再装进内存:给装载留一点时间。
        time.sleep(2)
        status, _ = api("POST", "/api/auth/login", {"username": "miyu", "password": "miyu"})
        assert status == 204, f"builtin login {status}"
        catalogue = json.loads(cache_file.read_text("utf-8"))
        first_window = catalogue_window(catalogue, FIRST)
        added_window = catalogue_window(catalogue, ADDED)
        check("目录里有这两个模型的窗口(测具前提)", bool(first_window and added_window),
              f"{FIRST}={first_window} {ADDED}={added_window}")

        status, before = current_context()
        check("启动时的模型查得到窗口", status == 200 and before.get("context_window_assumed") is False
              and before.get("context_window") == first_window, f"{status} {before}")

        status, cfg = api("GET", "/api/config")
        assert status == 200, cfg
        config = cfg["config"]
        provider = next(item for item in config["providers"] if item["id"] == PROVIDER)
        provider["models"] = [FIRST, ADDED]
        config["active_provider_models"] = [{"provider_id": PROVIDER, "model": ADDED}]
        status, body = api("PUT", "/api/config", {"config": config, "prompts": cfg["prompts"]})
        check("设置页保存成功", status == 200, f"{status} {str(body)[:200]}")

        status, after = current_context()
        check("新加的模型查得到窗口(不是退回的默认值)",
              status == 200 and after.get("context_window_assumed") is False
              and after.get("context_window") == added_window, f"{status} {after}")
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
    print(f"{sum(RESULTS)}/{len(RESULTS)} passed")
    return 0 if all(RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())

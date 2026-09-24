#!/usr/bin/env python3
"""网页两处小 bug 的走查（09-24）：任务清单那一步漏出内部标记、提问面板页码写死英文。

- todowrite 的进度消息是给终端画表用的 `__todo_table__{整张清单的 JSON}`。网页没拦，
  那一步没有主语时拿它当主语，整段 JSON 印在时间线上。修后主语是「3 项」，刷新之后
  （从库里回放）也一样。
- 提问面板的页码写死「1 of N」，中文界面也是英文（i18n 门禁只查中文字符串）。修后是
  「1 / 2」，翻到下一题是「2 / 2」。

前端由 Playwright 拦下换成 `WEB` 目录里的文件，二进制不用重编；`WEB` 指向改之前的
前端就能看到修前的红。

    BIN=<miyu 二进制> [WEB=<web 目录>] python3 testkit/webui-fixes/todo_and_question.py
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

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import authlib  # noqa: E402

BIN = Path(os.environ["BIN"]).expanduser().resolve()
WEB = Path(os.environ.get("WEB", HERE.parent.parent / "web")).resolve()
OUT = Path(os.environ.get("OUT", "~/.cache/miyu-webui-todo-question")).expanduser()
HOME = OUT / "home"
RUNTIME = OUT / "runtime"
PORT = int(os.environ.get("PORT", "18541"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18542"))
BASE = f"http://127.0.0.1:{PORT}"

TODOS = [
    {"content": "给 load() 加进程内缓存", "status": "in_progress", "priority": "high"},
    {"content": "配置文件改动时自动失效", "status": "pending", "priority": "medium"},
    {"content": "重新测一遍加载耗时", "status": "pending", "priority": "medium"},
]
QUESTIONS = [
    {"header": "周末", "question": "这个周末更想怎么过？",
     "options": [{"label": "出门走走", "description": "公园、展览"},
                 {"label": "在家休息", "description": "看书、打游戏"}]},
    {"header": "预算", "question": "预算大概多少？",
     "options": [{"label": "两百以内", "description": "简单吃一顿"},
                 {"label": "不设上限", "description": "好好犒劳自己"}]},
]
# 关键词 → 各步。第几步 = 那条用户消息之后已有几条工具结果。
PLAYS = {
    "列个清单": [
        {"tool": ("todowrite", {"todos": TODOS})},
        {"content": "清单列好了。"},
    ],
    "问我两个问题": [
        {"tool": ("ask_question", {"questions": QUESTIONS})},
        {"content": "好，按你的回答来安排。"},
    ],
}


class Stub(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _sse(self, payload):
        self.wfile.write(f"data: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
        self.wfile.flush()

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", "0")))
        messages = json.loads(body or b"{}").get("messages", [])
        step = {"content": "好。"}
        for index in range(len(messages) - 1, -1, -1):
            if messages[index].get("role") != "user":
                continue
            text = json.dumps(messages[index].get("content"), ensure_ascii=False)
            key = next((key for key in PLAYS if key in text), None)
            if key:
                done = sum(1 for m in messages[index + 1:] if m.get("role") == "tool")
                steps = PLAYS[key]
                step = steps[min(done, len(steps) - 1)]
                break
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        if "tool" in step:
            name, arguments = step["tool"]
            self._sse({"choices": [{"index": 0, "delta": {"tool_calls": [{
                "index": 0, "id": f"call_{name}", "type": "function",
                "function": {"name": name, "arguments": json.dumps(arguments, ensure_ascii=False)},
            }]}, "finish_reason": None}]})
            finish = "tool_calls"
        else:
            self._sse({"choices": [{"index": 0, "delta": {"content": step["content"]}, "finish_reason": None}]})
            finish = "stop"
        self._sse({"choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                   "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}})
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def do_GET(self):
        data = json.dumps({"data": [{"id": "stub-model"}]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{"id": "stub", "display_name": "Stub", "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
                       "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model"]}],
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


def serve_local(route):
    name = route.request.url.split("?")[0].rsplit("/", 1)[-1] or "index.html"
    local = WEB / name
    ctype = {"html": "text/html; charset=utf-8", "js": "application/javascript; charset=utf-8",
             "css": "text/css; charset=utf-8"}.get(name.rsplit(".", 1)[-1] if "." in name else "")
    if local.exists() and ctype:
        route.fulfill(status=200, body=local.read_bytes(), headers={"content-type": ctype, "cache-control": "no-store"})
    else:
        route.continue_()


TODO_ROW = """() => {
  const cards = [...document.querySelectorAll('.tool-card')];
  const card = cards.find((c) => /任务|todo/i.test(c.querySelector('.tool-title')?.textContent || ''));
  return card ? (card.querySelector('.tool-summary')?.textContent || card.textContent).trim() : null;
}"""


def stored_rows_with(needle):
    """沙箱会话库里含 needle 的行数。拷 db/-wal/-shm 三件出来查，不碰活库。"""
    import sqlite3

    copy = OUT / "dbcopy"
    shutil.rmtree(copy, ignore_errors=True)
    copy.mkdir()
    db = next(HOME.rglob("conversation.db"))
    for suffix in ("", "-wal", "-shm"):
        source = db.with_name(db.name + suffix)
        if source.exists():
            shutil.copy(source, copy / source.name)
    con = sqlite3.connect(copy / db.name)
    hits = 0
    for (table,) in con.execute("select name from sqlite_master where type='table'"):
        for column in [row[1] for row in con.execute(f"pragma table_info('{table}')")]:
            try:
                hits += con.execute(
                    f"select count(*) from '{table}' where CAST(\"{column}\" AS TEXT) like ?",
                    (f"%{needle}%",)).fetchone()[0]
            except sqlite3.Error:
                pass
    con.close()
    return hits


def send(page, text):
    page.fill("#composerInput", text)
    page.press("#composerInput", "Enter")


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    HOME.mkdir(parents=True)
    RUNTIME.mkdir(parents=True)
    write_config()
    stub = ThreadingHTTPServer(("127.0.0.1", STUB_PORT), Stub)
    threading.Thread(target=stub.serve_forever, daemon=True).start()
    daemon = subprocess.Popen([str(BIN), "__daemon", "--port", str(PORT)],
                              env=dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME)),
                              cwd=str(HOME), stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
    report = {"web": str(WEB)}
    try:
        assert wait_http(f"{BASE}/api/health"), "daemon not up"
        authlib.bootstrap(BASE)
        time.sleep(1)
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page(viewport={"width": 1280, "height": 900})
            page.route(lambda u: u.startswith(BASE) and (u.rstrip("/") == BASE or any(
                k in u for k in ("/app.js", "/styles.css", "/index.html", "/i18n-en.js"))), serve_local)
            page.goto(BASE)
            authlib.ui_login(page)
            page.wait_for_selector("#composerInput:not([disabled])", timeout=20000)
            page.wait_for_timeout(600)

            send(page, "帮我列个清单")
            page.wait_for_function("() => document.body.innerText.includes('清单列好了')", timeout=30000)
            page.wait_for_timeout(800)
            live = page.evaluate(TODO_ROW)
            report["_todo_row_live"] = live
            report["todo_row_has_no_marker"] = live is not None and "__todo_table__" not in live
            report["todo_row_says_three_items"] = live is not None and "3 项" in live
            page.screenshot(path=str(OUT / "todo-live.png"))

            # 刷新后的回放按库里的这一轮重画。标记只在进度消息里、不落库，回放本来漏
            # 不出来；这里钉住「库里没有它」——哪天进度也落库了，回放那条路也得拦。
            report["stored_turn_has_no_marker"] = not stored_rows_with("__todo_table__")

            send(page, "问我两个问题再定计划")
            page.wait_for_selector(".question-position", timeout=30000)
            page.wait_for_timeout(400)
            first = page.inner_text(".question-position").strip()
            report["_position_first"] = first
            report["position_first_is_neutral"] = first == "1 / 2"
            page.screenshot(path=str(OUT / "question.png"))
            # 选中一个选项后面板自己翻到下一题（autoAdvanceTimer）。
            page.evaluate("""() => {
              const page = [...document.querySelectorAll('.question-options')].find((el) => el.offsetParent);
              page?.querySelector('.question-option')?.click();
            }""")
            page.wait_for_timeout(1200)
            second = page.inner_text(".question-position").strip()
            report["_position_second"] = second
            report["position_second_is_neutral"] = second == "2 / 2"
            browser.close()
    finally:
        stub.shutdown()
        if daemon.poll() is None:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
    return report


if __name__ == "__main__":
    result = main()
    print(json.dumps(result, ensure_ascii=False, indent=2))
    bad = [k for k, v in result.items() if v is False]
    print("通过" if not bad else f"红: {bad}")
    print("产物：", OUT)

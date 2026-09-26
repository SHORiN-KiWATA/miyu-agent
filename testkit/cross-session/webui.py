#!/usr/bin/env python3
"""跨会话消息的 WebUI 走查(09-23):沙箱 daemon + stub.py + Playwright(Chromium)。

浏览器开着「查资料」(B)——网页心跳让它算作开着;「写代码」(A)经接口起一轮,
桩模型调 send_to_other_running_session 把一段 14 行的消息发给 B。判据:

    web_presence_counts          B 只靠网页心跳开着,A 发得过去(发不过去工具会报错)
    received_block_live          B 那边实时出现「从 写代码（短 id）收到消息」那一块,并进紧跟着的 AI
                                 回复、排在最前面(09-24 定的版式),正文露不全、有 ⋮
    received_block_expands       点一下看全文(⋮ 消失、最后一行看得见),再点收回去
    received_block_after_reload  刷新之后回看,还是那一块(不是用户气泡、不露外壳)
    send_card_preview            A 那边的工具签:抬头右边是「<B 的 id> 查资料」,底下露正文前几行
    send_card_expands            点正文预览 = 展开这张签,展开区顶上是正文全文、没有裸参数
    list_step_titled_live        A 里让 AI 列名单:那张工具签抬头叫「列出其他会话」,右边不再挂说明(09-26)
    list_step_titled_after_reload  刷新之后回看还是这样
    settings_has_preview_lines   设置页有「跨会话AI消息预览行数」这一项

截图落在 OUT(默认 /tmp/miyu-xs-webui),看完手动删。

    python3 testkit/cross-session/webui.py <miyu 二进制>
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

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "webui-fixes"))
import authlib  # noqa: E402

OUT = Path(os.environ.get("OUT", "/tmp/miyu-xs-webui"))
# 前端从工作树读(改 js/css 不必重编二进制);首页仍由服务端出——界面语言是它注入的。
WEB = Path(os.environ.get("WEB", HERE.parent.parent / "web")).resolve()
LOCAL_ASSETS = ("app.js", "styles.css", "crosssession.js", "settings-schema.js", "i18n-en.js")
HOME = OUT / "home"
RUNTIME = OUT / "run"
# 容器里完整露出来的行数(按行盒数,同 crosssession.js 的量法)。
VISIBLE_LINES_JS = """(body) => {
  const box = body.getBoundingClientRect();
  const range = document.createRange();
  const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
  const rects = [];
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (!n.textContent.trim()) continue;
    range.selectNodeContents(n);
    for (const r of range.getClientRects()) if (r.height > 0) rects.push(r);
  }
  rects.sort((a, b) => a.top - b.top);
  // 同一行里几段的上沿参差(行内代码),落在上一行底边之上的归同一行。
  let lines = 0;
  let lineBottom = -Infinity;
  const shown = [];
  for (const r of rects) {
    if (r.top < lineBottom - 1) { lineBottom = Math.max(lineBottom, r.bottom); shown[shown.length - 1] = lineBottom; continue; }
    lineBottom = r.bottom;
    shown.push(lineBottom);
  }
  return shown.filter((bottom) => bottom <= box.bottom + 1).length;
}"""
MESSAGE = "\n".join(
    [f"第 {index} 行：**构建**好了，`cargo test` 过了" for index in range(1, 15)]
)


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


PORT = free_port()
STUB_PORT = free_port()
BASE = f"http://127.0.0.1:{PORT}"


def clean_env(**extra):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("HERDR_")
           and k not in ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "MIYU_HOME")}
    env.update(extra)
    return env


def write_config():
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "config_version": 3,
        "oobe_done": True,
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{
            "id": "stub", "display_name": "Stub", "enabled": True,
            "base_url": f"http://127.0.0.1:{STUB_PORT}/v1", "protocol": "openai-chat",
            "api_key": "stub-key", "models": ["stub-model"], "default_model": "stub-model",
        }],
        "memory": {"enabled": False},
        "voice": {"enabled": False},
    }
    (HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False), "utf-8")


def wait_http(url, timeout=40):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


def api(method, path, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers={"content-type": "application/json"})
    with authlib.OPENER.open(req, timeout=15) as resp:
        raw = resp.read()
        return json.loads(raw) if raw else {}


def serve_local(route):
    name = route.request.url.split("?")[0].rsplit("/", 1)[-1]
    local = WEB / name
    ctype = "text/css; charset=utf-8" if name.endswith(".css") else "application/javascript; charset=utf-8"
    route.fulfill(status=200, body=local.read_bytes(), headers={"content-type": ctype, "cache-control": "no-store"})


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    binary = Path(sys.argv[1]).resolve()
    if OUT.exists():
        shutil.rmtree(OUT)
    RUNTIME.mkdir(parents=True)
    write_config()
    results = {}

    def check(name, ok, detail=""):
        results[name] = bool(ok)
        print(f"{'✅' if ok else '❌'} {name}  {str(detail)[:1600]}", flush=True)

    stub = subprocess.Popen([sys.executable, str(HERE / "stub.py")],
                            env=clean_env(STUB_PORT=str(STUB_PORT), STUB_LOG=str(OUT / "stub.jsonl")),
                            stdout=subprocess.DEVNULL, stderr=(OUT / "stub.err").open("w"))
    daemon = None
    try:
        assert wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models"), "stub not up"
        daemon = subprocess.Popen([str(binary), "__daemon", "--port", str(PORT)],
                                  env=clean_env(MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=str(RUNTIME), LANG="zh_CN.UTF-8"),
                                  cwd=str(HOME), stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
        assert wait_http(f"{BASE}/api/health"), "daemon not up"
        authlib.bootstrap(BASE)
        sender = api("POST", "/api/sessions", {"name": "写代码"})["session"]["session_id"]
        target = api("POST", "/api/sessions", {"name": "查资料"})["session"]["session_id"]
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            # 中文界面:用户用的是中文;无头 Chromium 默认报英文,界面会跟着换成英文。
            page = browser.new_page(viewport={"width": 1280, "height": 900}, locale="zh-CN")
            page.route(
                lambda url: url.startswith(BASE)
                and url.split("?")[0].rsplit("/", 1)[-1] in LOCAL_ASSETS,
                serve_local,
            )
            page.goto(BASE)
            authlib.ui_login(page)
            page.wait_for_selector("#composerInput:not([disabled])", timeout=20000)

            def open_session(session_id):
                page.click(f'.session-item[data-session-id="{session_id}"] .session-item-main')
                page.wait_for_timeout(1500)

            open_session(target)
            # 心跳一秒一查:换到 B 之后等它报上去。
            page.wait_for_timeout(2500)
            api("POST", "/api/turns", {"session_id": sender, "content": f"TK send {target} {MESSAGE}"})
            try:
                page.wait_for_selector(".xs-message", timeout=30000)
                live_ok = True
            except Exception:
                live_ok = False
            page.wait_for_timeout(2500)
            sender_reply = ""
            for turn in api("GET", f"/api/sessions/{sender}/turns").get("turns", []):
                sender_reply = str(turn.get("assistant_content") or sender_reply)
            check("web_presence_counts", '"delivered": "started a new turn there"' in sender_reply, sender_reply[:200])
            state = page.evaluate("""() => {
              const node = document.querySelector('.xs-message');
              if (!node) return null;
              const body = node.querySelector('.xs-body');
              const more = node.querySelector('.xs-more');
              return {
                head: node.querySelector('.xs-head')?.innerText || '',
                clipped: node.classList.contains('is-clipped'),
                moreVisible: more ? !more.hidden : false,
                bodyText: body?.innerText || '',
                strong: !!body?.querySelector('strong'),
                bodyColor: body ? getComputedStyle(body.querySelector('p') || body).color : '',
                code: !!body?.querySelector('code'),
                userBubbles: document.querySelectorAll('.user-message').length,
                // 09-24 定的版式:并进紧跟着的那段 AI 回复,排在最前面。
                inReply: !!node.closest('.assistant-message .assistant-blocks'),
                firstInReply: node.parentElement?.firstElementChild === node,
                rawTag: document.getElementById('chatScroll')?.innerText.includes('<cross-session-message') || false,
              };
            }""")
            shown = page.eval_on_selector(".xs-message .xs-body", VISIBLE_LINES_JS)
            check("received_block_live",
                  live_ok and state and f"从 写代码（{sender.rsplit('_', 1)[-1]}）收到消息" in state["head"]
                  and state["clipped"]
                  and state["moreVisible"] and state["strong"] and state["code"] and not state["rawTag"]
                  and state["inReply"] and state["firstInReply"]
                  and shown == 10,
                  {**(state or {}), "bodyText": "…", "shown_lines": shown})
            if os.environ.get("XS_DEBUG"):
                print(page.evaluate("""() => {
                  const body = document.querySelector('.xs-message .xs-body');
                  const p = body.querySelector('p');
                  const cs = (el) => { const s = getComputedStyle(el); return [s.fontSize, s.lineHeight, s.maxHeight, s.marginTop, s.marginBottom]; };
                  const top = body.getBoundingClientRect().top;
                  const range = document.createRange();
                  const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
                  const rects = [];
                  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
                    if (!n.textContent.trim()) continue;
                    range.selectNodeContents(n);
                    for (const r of range.getClientRects()) if (r.height > 0) rects.push([Math.round(r.top - top), Math.round(r.bottom - top), n.textContent.slice(0, 8)]);
                  }
                  rects.sort((a, b) => a[0] - b[0]);
                  return {body: cs(body), clip: body.style.getPropertyValue('--xs-clip'), bodyH: body.clientHeight, rects: rects.slice(0, 14)};
                }"""))
            page.screenshot(path=str(OUT / "b-received.png"))
            page.click(".xs-message .xs-body")
            page.wait_for_timeout(400)
            expanded = page.evaluate("""() => {
              const node = document.querySelector('.xs-message');
              const body = node.querySelector('.xs-body');
              const last = [...body.querySelectorAll('p')].pop();
              const box = body.getBoundingClientRect();
              const lastBox = last?.getBoundingClientRect();
              return {
                bodyColor: getComputedStyle(body.querySelector('p') || body).color,
                expanded: node.classList.contains('is-expanded'),
                moreHidden: node.querySelector('.xs-more').hidden,
                lastVisible: !!lastBox && lastBox.bottom <= box.bottom + 1,
                lastText: last?.innerText || '',
              };
            }""")
            page.screenshot(path=str(OUT / "b-expanded.png"))
            page.click(".xs-message .xs-body")
            page.wait_for_timeout(400)
            collapsed_again = page.evaluate("() => !document.querySelector('.xs-message').classList.contains('is-expanded')")
            # 收着时露的那几行是附注,颜色要比点开的全文淡(用户 09-24「消息预览的颜色不对」)。
            check("received_block_expands",
                  expanded["expanded"] and expanded["moreHidden"] and expanded["lastVisible"]
                  and "第 14 行" in expanded["lastText"] and collapsed_again
                  and state and state["bodyColor"] and state["bodyColor"] != expanded["bodyColor"],
                  {**{k: v for k, v in expanded.items() if k != "lastText"}, "collapsed_color": state and state["bodyColor"],
                   "collapsed_again": collapsed_again})

            page.reload()
            page.wait_for_selector("#composerInput:not([disabled])", timeout=20000)
            open_session(target)
            page.wait_for_timeout(1500)
            replay = page.evaluate("""() => ({
              blocks: document.querySelectorAll('.xs-message').length,
              inReply: !!document.querySelector('.assistant-message .assistant-blocks > .xs-message:first-child'),
              head: document.querySelector('.xs-message .xs-head')?.innerText || '',
              rawTag: document.getElementById('chatScroll')?.innerText.includes('<cross-session-message') || false,
            })""")
            check("received_block_after_reload",
                  replay["blocks"] == 1 and replay["inReply"] and "从 写代码（" in replay["head"]
                  and not replay["rawTag"], replay)

            open_session(sender)
            page.wait_for_selector(".tool-card.is-xs-send", state="attached", timeout=15000)
            # 过程跑完按默认收成一行 `Worked for …`(和别的工具一样),先点开它。
            page.wait_for_timeout(800)
            if os.environ.get("XS_DEBUG"):
                print(page.evaluate("""() => [...document.querySelectorAll('.proc-line')].map((line) => {
                  const r = line.getBoundingClientRect();
                  const h = line.querySelector(':scope > .proc-head');
                  const hr = h?.getBoundingClientRect();
                  return {cls: line.className, top: Math.round(r.top), h: Math.round(r.height),
                          head: hr ? [Math.round(hr.top), Math.round(hr.height), h.hidden] : null,
                          send: !!line.querySelector('.tool-card.is-xs-send')};
                })"""))
                page.screenshot(path=str(OUT / "a-before-fold.png"))
            # 只有左边的箭头和那段文字接点击(整行空白处故意不接),点文字。
            fold = page.locator(".proc-line:has(.tool-card.is-xs-send):not(.is-open) > .proc-head .proc-summary")
            if fold.count():
                fold.first.click()
                page.wait_for_timeout(600)
            card = page.evaluate("""() => {
              const card = document.querySelector('.tool-card.is-xs-send');
              const preview = card.querySelector(':scope > .xs-send-preview');
              return {
                summary: card.querySelector('.tool-summary')?.innerText || '',
                title: card.querySelector('.tool-title strong')?.innerText || '',
                previewVisible: !!preview && preview.getBoundingClientRect().height > 0,
                moreVisible: !card.querySelector(':scope > .xs-send-more').hidden,
                previewText: preview?.innerText || '',
              };
            }""")
            send_shown = page.eval_on_selector(".tool-card.is-xs-send > .xs-send-preview", VISIBLE_LINES_JS)
            check("send_card_preview",
                  f"{target.rsplit('_', 1)[-1]} 查资料" in card["summary"] and target not in card["summary"]
                  and card["previewVisible"]
                  and card["moreVisible"] and "第 1 行" in card["previewText"] and send_shown == 10,
                  {**card, "previewText": "…", "shown_lines": send_shown})
            page.screenshot(path=str(OUT / "a-send.png"))
            page.click(".tool-card.is-xs-send > .xs-send-preview")
            page.wait_for_timeout(500)
            opened = page.evaluate("""() => {
              const card = document.querySelector('.tool-card.is-xs-send');
              const preview = card.querySelector(':scope > .xs-send-preview');
              // 裸参数那栏(标签「参数」)该收起来;结果栏里本来就有 session_id,不算。
              const args = [...card.querySelectorAll('.tool-detail')].filter((d) => !d.hidden)
                .filter((d) => d.querySelector('.tool-detail-label')?.innerText.trim() === '参数')
                .map((d) => d.innerText).join('\\n');
              return {
                open: !card.classList.contains('collapsed'),
                previewHidden: preview.getBoundingClientRect().height === 0,
                full: card.querySelector('.xs-send-full')?.innerText || '',
                rawArgs: args.includes('"session_id"'),
              };
            }""")
            page.screenshot(path=str(OUT / "a-send-expanded.png"))
            check("send_card_expands",
                  opened["open"] and opened["previewHidden"] and "第 14 行" in opened["full"] and not opened["rawArgs"],
                  {k: (v[:80] if isinstance(v, str) else v) for k, v in opened.items()})

            # 列名单那一步(用户 09-26):抬头叫「列出其他会话」,右边不再挂「列出开着的会话」。
            # 收起的过程里卡片不可见,用 textContent 读。
            list_cards_js = """() => [...document.querySelectorAll('.tool-card')]
              .filter((card) => (card.querySelector('.tool-technical-name')?.textContent || '')
                .includes('send_to_other_running_session'))
              .map((card) => ({
                title: card.querySelector('.tool-title strong')?.textContent || '',
                summary: card.querySelector('.tool-summary')?.textContent || '',
              }))"""

            def completed(session_id):
                return sum(1 for turn in api("GET", f"/api/sessions/{session_id}/turns").get("turns", [])
                           if turn.get("status") == "completed")

            done_before = completed(sender)
            api("POST", "/api/turns", {"session_id": sender, "content": "TK list"})
            deadline = time.time() + 30
            while time.time() < deadline and completed(sender) <= done_before:
                time.sleep(0.3)
            page.wait_for_timeout(1500)

            def list_step_ok(cards):
                last = cards[-1] if cards else {}
                return (last.get("title") == "列出其他会话"
                        and "列出开着的会话" not in last.get("summary", "")
                        and "给其他会话发送消息" not in last.get("title", ""))

            live_cards = page.evaluate(list_cards_js)
            check("list_step_titled_live", len(live_cards) >= 2 and list_step_ok(live_cards), live_cards)
            page.reload()
            page.wait_for_selector("#composerInput:not([disabled])", timeout=20000)
            open_session(sender)
            page.wait_for_timeout(1500)
            replay_cards = page.evaluate(list_cards_js)
            check("list_step_titled_after_reload", len(replay_cards) >= 2 and list_step_ok(replay_cards),
                  replay_cards)

            schema = page.evaluate("""() => JSON.stringify(window.MiyuSettingsSchema || {})""")
            check("settings_has_preview_lines",
                  "display.cross_session_preview_lines" in schema and "跨会话AI消息预览行数" in schema, "")
            browser.close()
    except Exception as error:  # noqa: BLE001 — 走查中途出错也要记成失败
        check("aborted", False, repr(error)[:1500])
    finally:
        if daemon is not None:
            daemon.send_signal(signal.SIGTERM)
            try:
                daemon.wait(15)
            except subprocess.TimeoutExpired:
                daemon.kill()
        stub.terminate()
    failed = [name for name, ok in results.items() if not ok]
    print(f"\n{len(results) - len(failed)}/{len(results)} 通过" + (f",失败:{failed}" if failed else "")
          + f"  截图在 {OUT}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

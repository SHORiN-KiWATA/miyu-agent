#!/usr/bin/env python3
"""WebUI 行内 Markdown 的 DOM 断言,吃的是 `web/app.js` 里的真解析器。

WebUI 的 Markdown 是自写 DOM 解析(`appendInline` / `markdownTable` /
`renderMarkdown`),表头和表体共用同一个行内解析器。这份走查只关心它解析成了
什么节点,不关心配色和布局,所以不起 daemon、不连模型、不重新构建二进制——
把 app.js 原样灌进一个空白页,只把结尾的 `initialize()` 换成导出。

    python3 testkit/webui-markdown/run.py

需要 python 的 playwright + chromium。不花额度。
"""

import functools
import http.server
import os
import sys
import threading
from pathlib import Path
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
# MIYU_APP_JS 指向别的副本,用来跑「修之前会红」这一步:
#   git show HEAD:web/app.js > /tmp/before.js
#   MIYU_APP_JS=/tmp/before.js python3 testkit/webui-markdown/run.py
APP = Path(os.environ.get("MIYU_APP_JS", REPO / "web" / "app.js"))

# app.js 是个 IIFE,内部函数外面够不着。把末尾的启动调用换成导出:启动要真 DOM
# 和后端,这里两样都没有;函数本身一个字节没改。
ENTRY = "\n  initialize();\n})();"
EXPORT = "\n  window.__miyuMarkdown = { renderMarkdown, appendInline };\n})();"

# (名字, markdown, 断言函数)。断言收到的是 #root 的 DOM 探针结果。
CASES = [
    (
        "表格单元格里的 <br>",
        "| a | b |\n|---|---|\n| 上<br>下 | x |\n",
        {"tables": 1, "rows": 1, "cols": 2, "cellBr": 1, "cellText": "上下", "brInCode": 0},
    ),
    (
        "<br/> 与 <br /> 与大小写",
        "第一行<br/>第二行<br />第三行<BR>第四行\n",
        {"paraBr": 3, "text": "第一行第二行第三行第四行"},
    ),
    (
        "连着两个 br",
        "上<br><br>下\n",
        {"paraBr": 2},
    ),
    (
        "br 前后还有粗体和链接",
        "**粗**<br>[名字](https://example.com)\n",
        {"paraBr": 1, "strong": 1, "links": 1},
    ),
    (
        "带属性的 br 不是 br",
        "上<br class=x>下\n",
        {"paraBr": 0, "text": "上<br class=x>下"},
    ),
    (
        "别的标签仍然是文本",
        "<script>alert(1)</script><img src=x onerror=alert(1)>\n",
        {"scripts": 0, "imgs": 0, "paraBr": 0},
    ),
    (
        "行内代码里的 br 是字面量",
        "`上<br>下`\n",
        {"paraBr": 0, "codeText": "上<br>下", "brInCode": 0},
    ),
    (
        "围栏代码里的 br 是字面量",
        "```\n上<br>下\n```\n",
        {"paraBr": 0, "brInCode": 0, "preText": "上<br>下"},
    ),
    (
        "转义过的实体不再当标签认",
        "上&lt;br&gt;下\n",
        {"paraBr": 0, "text": "上&lt;br&gt;下"},
    ),
    (
        "流式没写完的标签先当文本",
        "上<br\n",
        {"paraBr": 0},
    ),
    (
        "尖括号 URL 照旧成链接",
        "看这个 <https://example.com> 谢谢\n",
        {"links": 1, "paraBr": 0},
    ),
]

PROBE = """(markdown) => {
  const root = document.getElementById('root');
  root.innerHTML = '';
  window.__miyuMarkdown.renderMarkdown(root, markdown);
  const table = root.querySelector('table');
  const cell = table ? table.querySelector('tbody td') : null;
  const para = root.querySelector('p');
  const code = root.querySelector('p code, li code');
  const pre = root.querySelector('pre code');
  return {
    tables: root.querySelectorAll('table').length,
    rows: table ? table.querySelectorAll('tbody tr').length : 0,
    cols: table ? table.querySelectorAll('thead th').length : 0,
    cellBr: cell ? cell.querySelectorAll('br').length : 0,
    cellText: cell ? cell.textContent : null,
    paraBr: para ? para.querySelectorAll('br').length : 0,
    text: para ? para.textContent : null,
    strong: root.querySelectorAll('strong').length,
    links: root.querySelectorAll('a').length,
    scripts: root.querySelectorAll('script').length,
    imgs: root.querySelectorAll('img').length,
    brInCode: root.querySelectorAll('code br, pre br').length,
    codeText: code ? code.textContent : null,
    preText: pre ? pre.textContent : null,
  };
}"""


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def main():
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        sys.exit("需要 playwright:pip install playwright && playwright install chromium")

    source = APP.read_text()
    if source.count(ENTRY) != 1:
        sys.exit(f"{APP} 的结尾不是预期的 initialize() 调用,导出方式要跟着改")
    harness = source.replace(ENTRY, EXPORT)

    # 得有个真 origin:about:blank 上 Chromium 不给碰 localStorage,而 app.js
    # 顶层就在读它。起个只服务这两个文件的本地 http。
    serve = sandbox_dir.make("miyu-md-")
    (serve / "app.js").write_text(harness)
    (serve / "index.html").write_text(
        "<!doctype html><meta charset=utf-8><div id=root></div><script src=app.js></script>"
    )
    handler = functools.partial(QuietHandler, directory=str(serve))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_address[1]}/index.html"

    failures = []
    with sync_playwright() as play:
        browser = play.chromium.launch()
        page = browser.new_page()
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.goto(base)
        if page.evaluate("() => !window.__miyuMarkdown"):
            browser.close()
            sys.exit("app.js 没能加载出解析器:\n" + "\n".join(errors))

        for name, markdown, expected in CASES:
            got = page.evaluate(PROBE, markdown)
            bad = {key: (want, got.get(key)) for key, want in expected.items()
                   if got.get(key) != want}
            print(f"{'✓' if not bad else '✗'} {name}")
            for key, (want, actual) in bad.items():
                print(f"    {key}: 期望 {want!r},实得 {actual!r}")
                failures.append(name)
        browser.close()
    server.shutdown()

    print()
    if failures:
        sys.exit(f"{len(set(failures))}/{len(CASES)} 项不符合预期")
    print(f"{len(CASES)}/{len(CASES)} 项符合预期")


if __name__ == "__main__":
    main()

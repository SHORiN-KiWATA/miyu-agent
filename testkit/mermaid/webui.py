#!/usr/bin/env python3
"""WebUI 的 ```mermaid 卡片走查：真 daemon + 真浏览器。

桩模型回一段里面塞三种围栏——能画的 mermaid、语法坏掉的 mermaid、一段 rust。
三种各有各的归宿，缺一不可：

1. 能画的 → 卡片里是真 `<svg>`（服务端 `POST /api/mermaid` 渲染的，和终端
   同一个 Rust 渲染器），源码默认折起来，点「源码」才展开；
2. 画不出来的 → 整张卡片换回普通代码块，源码原样可见（和终端退回围栏同规矩）；
3. rust 围栏 → 压根不进这条路。

还盯一条页面自己的错误：卡片里折着的那份源码要是走 `codeBlock()` 拿，会被那条
mermaid 分支再接回卡片，无限递归——第一版就是这么写的，页面直接 RangeError。
所以 `pageerror` 一个都不许有。

    cargo build    # 静态资源编进二进制，改了 JS/CSS 必须重新构建
    python3 testkit/mermaid/webui.py

截图落在 /tmp/miyu-mermaid-webui/。不花额度。
"""

import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
# 09-11 起 WebUI 永远要登录，登录那套在这儿：内置账号 → 建管理员 → 账号登录。
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "webui-fixes"))
import authlib  # noqa: E402
BIN = Path(os.environ.get("MIYU_BIN", REPO / "target" / "debug" / "miyu"))
HOME = Path("/tmp/miyu-mermaid-webui/home")
RUNTIME = "/tmp/mx-mermaid"
OUT = Path(os.environ.get("OUT", "/tmp/miyu-mermaid-webui"))
SMOKE = REPO / "testkit" / "repl-smoke"

GOOD = (
    "flowchart TD\n"
    "    A[用户提问] --> B{要用工具吗}\n"
    "    B -->|要| C[调用工具]\n"
    "    B -->|不要| D[直接回答]\n"
    "    C --> D"
)
# 语法坏掉的那份：`render_svg` 该给 422，卡片该整块退回代码块。
#
# 用 `subgraph` 不闭合（`UnclosedSubgraph`）而不是「方括号没配对」——渲染器对
# 后者是宽容的，照样给你画出一张图来（garbage in, garbage out），退路根本不会
# 触发。硬解析错误才是这条退路真正的入口。
BROKEN = "flowchart TD\n    subgraph 这个块忘了 end\n    A --> B"
# 一张高得会撞上卡片限高的图：WebUI 那边缩着放、点开看原图。
TALL = "flowchart TD\n" + "\n".join(
    f"    N{i}[步骤{i} 做一件事] --> N{i + 1}[步骤{i + 1} 做另一件事]" for i in range(14)
)
RUST = 'fn main() {\n    println!("hi");\n}'
REPLY = (
    f"先看流程：\n\n```mermaid\n{GOOD}\n```\n\n"
    f"这段画不出来：\n\n```mermaid\n{BROKEN}\n```\n\n"
    f"这张很高：\n\n```mermaid\n{TALL}\n```\n\n"
    f"再看代码：\n\n```rust\n{RUST}\n```\n"
)

PROBE = """
() => {
  // 整段助手输出，不是第一个 `.markdown-body`——正文会按段切成好几块，
  // 只看第一块会漏掉后面的卡片（实测三张图时漏了两张）。
  const body = document.querySelector(".assistant-content");
  if (!body || !body.querySelector(".markdown-body")) return { missing: true };
  const cards = Array.from(body.querySelectorAll(".mermaid-block"));
  const card = cards[0] || null;
  const figure = card ? card.querySelector(".mermaid-figure") : null;
  const svg = figure ? figure.querySelector("svg") : null;
  const source = card ? card.querySelector(".mermaid-source") : null;
  // 那张高图：卡片该把它缩住，并在工具栏说明白点开能看原图。
  const tall = cards[1] || null;
  const tallSvg = tall ? tall.querySelector(".mermaid-figure svg") : null;
  // 折在卡片里的那份源码本身也是 .code-block，别把它算成"普通代码块"。
  const plain = Array.from(
    body.querySelectorAll(".code-block:not(.mermaid-block):not(.mermaid-source)")
  );
  return {
    cards: cards.length,
    hasSvg: !!svg,
    svgWidth: svg ? Math.round(svg.getBoundingClientRect().width) : 0,
    svgHeight: svg ? Math.round(svg.getBoundingClientRect().height) : 0,
    svgText: svg ? svg.textContent : "",
    // 渲染器给的自然宽度。摘掉 width/height 属性的话 SVG 会撑满卡片，
    // 一张小流程图被放大好几倍——量这个就能把"拉伸"钉死。
    naturalWidth: svg ? Math.round(parseFloat(svg.getAttribute("width") || "0")) : 0,
    // 量拉伸要用**计算样式**：`.app-shell` 上有 `zoom: var(--ui-scale)`（默认
    // 1.1），getBoundingClientRect 量到的是缩放之后的，天生比自然宽大一成。
    cssWidth: svg ? Math.round(parseFloat(getComputedStyle(svg).width)) : 0,
    zoomable: !!(figure && figure.classList.contains("is-zoomable")),
    sourceHidden: source ? source.hidden : null,
    figureHidden: figure ? figure.hidden : null,
    tallNatural: tallSvg ? Math.round(parseFloat(tallSvg.getAttribute("height") || "0")) : 0,
    tallShown: tallSvg ? Math.round(tallSvg.getBoundingClientRect().height) : 0,
    tallScaledNote: tall ? (tall.querySelector(".code-toolbar span")?.textContent || "") : "",
    plainBlocks: plain.length,
    plainLangs: plain.map((b) => b.querySelector(".code-toolbar span")?.textContent || ""),
    plainText: plain.map((b) => b.querySelector("pre")?.textContent || ""),
    // 数不对时的现场：所有代码块的 class + 正文里有没有那几句引子。
    inventory: Array.from(body.querySelectorAll(".code-block")).map((b) => b.className),
    intros: ["先看流程", "这段画不出来", "这张很高", "再看代码"]
      .filter((s) => body.textContent.includes(s)),
  };
}
"""

TOGGLE = """
() => {
  const card = document.querySelector(".assistant-content .mermaid-block");
  const toggle = card.querySelector(".mermaid-source-toggle");
  const figure = card.querySelector(".mermaid-figure");
  // 「源码」两个字要在一行里。蹭 `.code-copy-button` 的定宽图标格子时它会断成
  // 「源 / 码」两行，工具栏跟着变高——量计算样式的高度就能钉死。
  const height = Math.round(parseFloat(getComputedStyle(toggle).height));
  toggle.click();
  const source = card.querySelector(".mermaid-source");
  return {
    hidden: source.hidden,
    // 源码要**替换掉**图，不是挂在图下面（用户 09-20）。
    figureHidden: figure.hidden,
    label: toggle.textContent.trim(),
    text: source.querySelector("pre")?.textContent || "",
    toggleHeight: height,
  };
}
"""


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_turn_done(base, session_id, timeout=60.0):
    """等这一轮真跑完：库里有回合、没有在跑的 run。

    睡固定 3 秒不够——桩模型吐完这段要 5 秒多，浏览器赶上回合还在跑，流式期间正文
    每来一段整块重建、跑完又整段重画一次，拿到的卡片句柄随时被换掉，截图报「不在
    DOM 里」（09-24 三跑一次）。
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        data = api(base, f"/api/sessions/{session_id}/turns") or {}
        if data.get("turns") and not data.get("runs") and not data.get("running_turn_id"):
            return True
        time.sleep(0.3)
    return False


def api(base, path, body=None, method="GET"):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(
        base + path,
        data=data,
        method=method,
        headers={"Content-Type": "application/json", "Origin": base},
    )
    # 带上登录 cookie 的 opener——裸 urlopen 一律 401。
    with authlib.OPENER.open(request, timeout=120) as response:
        return json.loads(response.read() or b"null")


def write_config(stub_port):
    (HOME / "config").mkdir(parents=True, exist_ok=True)
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(
            {
                "active_provider": "stub",
                "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
                "providers": [
                    {
                        "id": "stub",
                        "display_name": "Stub",
                        "base_url": f"http://127.0.0.1:{stub_port}/v1",
                        "protocol": "openai-chat",
                        "api_key": "stub",
                        "models": ["stub-model"],
                    }
                ],
                "memory": {"enabled": False},
                # 界面语言钉中文（判定比的是中文界面字；没写就跟浏览器走，无头 Chromium 是英文）。
                "display": {"language": "zh"},
                "tools": {"enabled": False},
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )


def wait_http(url, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.2)
    return False


def endpoint_checks(base, report):
    """接口自己的三条：画得出、画不出、太大。前端的退路全靠它们分得清。"""

    def post(source):
        data = json.dumps({"source": source}).encode()
        request = urllib.request.Request(
            f"{base}/api/mermaid",
            data=data,
            method="POST",
            headers={"Content-Type": "application/json", "Origin": base},
        )
        try:
            with authlib.OPENER.open(request, timeout=60) as response:
                return response.status, json.loads(response.read() or b"null")
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read() or b"null")

    status, data = post(GOOD)
    # 原样存一份：版式出问题时第一件事就是看这个根标签带没带尺寸。
    (OUT / "good.svg").write_text(str(data.get("svg", "")), encoding="utf-8")
    report["接口: 好图回 200 带 svg"] = status == 200 and str(data.get("svg", "")).startswith("<svg")
    report["接口: 中文标签进得了 svg"] = "用户提问" in str(data.get("svg", ""))
    status, data = post(BROKEN)
    report["接口: 坏图回 422 带原因"] = status == 422 and bool(data.get("error"))
    status, data = post("")
    report["接口: 空源码回 422"] = status == 422
    status, data = post("flowchart TD\n" + "    A --> B\n" * 40000)
    report["接口: 超长源码回 413"] = status == 413


def main():
    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        print("! 需要 playwright：pip install playwright && playwright install chromium",
              file=sys.stderr)
        return 2

    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    stub_port, port = free_port(), free_port()
    base = f"http://127.0.0.1:{port}"
    write_config(stub_port)

    report = {}
    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(stub_port), STUB_REPLY=REPLY),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    daemon = None
    try:
        if not wait_http(f"http://127.0.0.1:{stub_port}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(port)],
            env=dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME),
            cwd=str(HOME),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if not wait_http(f"{base}/api/config"):
            print("! daemon 没起来", file=sys.stderr)
            return 2

        authlib.bootstrap(base)
        endpoint_checks(base, report)

        # WebUI 有意不显示「终端集成会话」，得自己建一条。
        session = api(base, "/api/sessions", {"name": "图表走查", "switch": True}, "POST")
        session_id = session.get("session_id") or session.get("session", {}).get("session_id")
        api(base, "/api/turns", {"content": "画个图", "session_id": session_id}, "POST")
        wait_turn_done(base, session_id)

        with sync_playwright() as play:
            browser = play.chromium.launch()
            # 界面按中文：判定比的是中文界面字，无头 Chromium 默认英文（09-23 网页双语起）。
            page = browser.new_page(viewport={"width": 1280, "height": 900}, locale="zh-CN")
            errors = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.goto(base)
            authlib.ui_login(page)
            page.wait_for_selector(".assistant-content .markdown-body", timeout=30000)
            # 等**每一张**卡片有结果：要么画出了 svg，要么退回成代码块。
            # 睡固定秒数不行——一屏三张图时后两张还在路上就被拍了照（实测）。
            try:
                page.wait_for_function(
                    """() => {
                      const cards = document.querySelectorAll(
                        '.assistant-content .mermaid-block');
                      return cards.length > 0 && [...cards].every(
                        (card) => card.querySelector('.mermaid-figure svg'));
                    }""",
                    timeout=30000,
                )
            except Exception:
                pass
            time.sleep(0.5)
            got = page.evaluate(PROBE)
            page.screenshot(path=str(OUT / "page.png"), full_page=True)
            # 单独给卡片来一张：整页图里它多半被滚出视口，看版式得看这张。
            # 用 locator 不用元素句柄：每次截图都重新找，正文重画过也拍得到。
            card = page.locator(".assistant-content .mermaid-block").first
            if card.count():
                card.screenshot(path=str(OUT / "card.png"))

            report["页面没有报错"] = not errors
            if errors:
                print("  页面报错：" + " | ".join(errors[:3]))
            report["剩两张图表卡片（坏的那张退回去了）"] = got.get("cards") == 2
            report["卡片里是真 svg"] = bool(got.get("hasSvg"))
            report["svg 有真实尺寸"] = got.get("svgWidth", 0) > 50 and got.get("svgHeight", 0) > 50
            report["图里有中文节点名"] = "用户提问" in str(got.get("svgText", ""))
            natural = got.get("naturalWidth", 0)
            report["图按自然尺寸画，没被拉伸"] = natural > 0 and got.get("cssWidth", 0) <= natural + 2
            print(f"  （svg 自然宽 {natural}，样式宽 {got.get('cssWidth')}，"
                  f"上屏 {got.get('svgWidth')}×{got.get('svgHeight')}）")
            report["图可点开放大"] = bool(got.get("zoomable"))
            report["源码默认折着"] = got.get("sourceHidden") is True
            # 坏掉那张退回代码块，加上 rust 那块，普通代码块应有两块。
            report["退回去的和 rust 各是一块普通代码块"] = got.get("plainBlocks") == 2
            report["图默认露在外面"] = got.get("figureHidden") is False
            natural_tall, shown_tall = got.get("tallNatural", 0), got.get("tallShown", 0)
            report["高图被卡片限住了"] = natural_tall > 600 and 0 < shown_tall < natural_tall - 2
            report["缩了就在工具栏说一句"] = "点开看原图" in str(got.get("tallScaledNote", ""))
            print(f"  （高图自然 {natural_tall}px，卡片里 {shown_tall}px）")
            print(f"  （代码块清单 {got.get('inventory')}）")
            print(f"  （正文里的引子 {got.get('intros')}）")
            report["坏掉那张的源码一字不少"] = any(
                BROKEN in text for text in got.get("plainText", [])
            )
            report["rust 围栏没被当成图"] = any(
                RUST in text for text in got.get("plainText", [])
            )

            toggled = page.evaluate(TOGGLE)
            if card.count():
                card.screenshot(path=str(OUT / "card-source-open.png"))
            report["点「源码」切得过去"] = toggled.get("hidden") is False
            report["切到源码时图让位（不是挂在下面）"] = toggled.get("figureHidden") is True
            report["按钮改写成「图表」（点了会切回去）"] = toggled.get("label") == "图表"
            report["展开的是原样源码"] = GOOD in str(toggled.get("text", ""))
            report["「源码」按钮没被挤成两行"] = 0 < toggled.get("toggleHeight", 0) <= 26
            browser.close()
    finally:
        for process in (daemon, stub):
            if process is None:
                continue
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()

    (OUT / "report.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    passed = sum(1 for ok in report.values() if ok)
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{passed}/{len(report)} passed  截图在 {OUT}")
    return 0 if report and passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())

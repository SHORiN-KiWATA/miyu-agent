#!/usr/bin/env python3
"""会话钉的模型没了：退回全局池、说一声、别把 REPL 整个锁死。

09-18 真机撞到的那条：会话钉着 `opencodego / union-alpha`，供应商清单里早没了，
于是 `miyu` 整个进不去、只报一句「invalid admin response」——daemon 侧给这条会话
装 Agent 估上下文时报「没有可用端点」，`?` 一路冒到顶、连接直接断掉。

这里验三件事：

1. 钉的模型失效 → REPL 照常开，底栏是全局池那个模型，屏上有一行说明；
2. 那份失效的覆盖被清掉了（不清的话每轮重踩一次、`/models` 里还显示着它），
   再开一次不再提示；
3. 一半失效时只筛掉失效那条，覆盖留着。

「daemon 出错要回一帧带原因的 Error」那条在这里验不了：全局池写成不存在的模型时
配置层会自己纠回去（实测底栏照样是 stub-model），逼不出真错误。那条走单测
`web::tests::ipc_bridge::a_handler_error_comes_back_as_an_error_frame`。

跑法：

    cargo build
    python3 testkit/tui/stale_model.py

产物在 ~/.cache/miyu-stale-model/。

**这些 TUI 走查只能一个一个跑**：共用同一个 `MIYU_HOME` 和桩模型端口。
"""

import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402

OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-stale-model"))
PROMPT = "走查一句"
GHOST = "miyu-model-the-provider-removed"


def write_config():
    (h.HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{
            "id": "stub",
            "display_name": "Stub",
            "base_url": f"http://127.0.0.1:{h.STUB_PORT}/v1",
            "protocol": "openai-chat",
            "api_key": "stub",
            "models": ["stub-model"],
        }],
        "memory": {"enabled": False},
    }
    (h.HOME / "config" / "config.jsonc").write_text(
        json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def conversation_db():
    found = sorted(h.HOME.glob("home/*/conversation.db"))
    assert found, "会话库还没建出来"
    return found[0]


def repl_lane_session():
    """普通车道指针指着的那条会话（`app_state` 里的 `repl_session_persona:*`）。"""
    with sqlite3.connect(f"file:{conversation_db()}?mode=ro", uri=True) as db:
        rows = db.execute(
            "SELECT key, value FROM app_state WHERE key LIKE 'repl_session_persona:%'"
        ).fetchall()
    assert rows, "还没有 REPL 车道指针"
    return rows[0][1]


def pin(session_id, models):
    encoded = json.dumps(models) if models else None
    with sqlite3.connect(conversation_db()) as db:
        db.execute(
            "UPDATE sessions SET model_override = ? WHERE session_id = ?",
            (encoded, session_id),
        )


def pinned(session_id):
    with sqlite3.connect(f"file:{conversation_db()}?mode=ro", uri=True) as db:
        row = db.execute(
            "SELECT model_override FROM sessions WHERE session_id = ?", (session_id,)
        ).fetchone()
    return row[0] if row else None


def run_tui(send=None, tag=""):
    """起一个 TUI、看几秒（可选发一句话）、返回还原出来的屏幕与原始字节。"""
    tui, master = h.spawn_tui()
    sink = bytearray()
    try:
        h.drain(master, 4.0, sink)
        if send:
            os.write(master, send.encode())
            h.drain_until(master, sink, send, 3.0)
            os.write(master, b"\r")
            h.drain_until(master, sink, "走查的回复", 40.0)
        h.drain(master, 1.5, sink)
    finally:
        try:
            os.write(master, b"\x03")
            time.sleep(0.3)
        except OSError:
            pass
        tui.terminate()
        try:
            tui.wait(timeout=5)
        except subprocess.TimeoutExpired:
            tui.kill()
        os.close(master)
    raw = bytes(sink)
    if tag:
        (OUT / f"{tag}.txt").write_text("\n".join(h.render(raw)), encoding="utf-8")
    return h.render(raw), raw


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

    stub = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT)),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = None
    report = {}
    try:
        if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
            print("! 桩模型没起来", file=sys.stderr)
            return 2

        def start_daemon():
            process = subprocess.Popen(
                [str(h.BIN), "__daemon", "--port", str(h.PORT)],
                env=h.ENV, cwd=str(h.HOME),
                stdout=(OUT / "daemon.log").open("a"), stderr=subprocess.STDOUT,
            )
            assert h.wait_http(f"{h.BASE}/api/config", timeout=30), "daemon 没起来"
            return process

        daemon = start_daemon()

        # ── 0. 先跑一轮，把普通车道那条会话建出来 ────────────────────────
        screen, _ = run_tui(send=PROMPT, tag="00-first-run")
        report["起头能正常跑一轮"] = any("走查的回复" in line for line in screen)
        session_id = repl_lane_session()

        # ── 1. 给它钉一个供应商已经没有的模型 ───────────────────────────
        pin(session_id, [{"provider_id": "stub", "model": GHOST}])
        screen, raw = run_tui(tag="01-stale-pin")
        text = "\n".join(screen)
        report["钉了失效模型也进得去"] = "invalid admin response" not in raw.decode(
            "utf-8", "replace"
        )
        report["屏上说清了是哪一条失效"] = GHOST in text and "退回全局模型池" in text
        report["底栏是全局池那个模型"] = "stub-model" in text
        report["失效的覆盖被清掉了"] = pinned(session_id) is None

        # ── 2. 清掉之后不再提示，而且照样能跑一轮（回合路也退回了全局池）──
        screen, _ = run_tui(send=PROMPT, tag="02-after-clear")
        text = "\n".join(screen)
        report["清掉后不再提示"] = GHOST not in text
        report["清掉后照样跑得完一轮"] = any("走查的回复" in line for line in screen)

        # ── 3. 一半失效：剩下能用的那条照钉，不清、不提示 ────────────────
        pin(session_id, [
            {"provider_id": "stub", "model": GHOST},
            {"provider_id": "stub", "model": "stub-model"},
        ])
        screen, _ = run_tui(send=PROMPT, tag="03-partly-stale")
        report["一半失效时不清覆盖"] = pinned(session_id) is not None
        report["一半失效时不提示"] = GHOST not in "\n".join(screen)
        # 回合路（`turns/task.rs`）套的是同一道守卫：钉着一条失效的也得跑得完。
        report["一半失效时回合照跑"] = any("走查的回复" in line for line in screen)
        pin(session_id, None)

    finally:
        for process in (daemon, stub):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=10)
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

#!/usr/bin/env python3
"""会话钉的模型没了：退回全局池、说一声、别把 REPL 整个锁死。

09-18 真机撞到的那条：会话钉着 `opencodego / union-alpha`，供应商清单里早没了，
于是 `miyu` 整个进不去、只报一句「invalid admin response」——daemon 侧给这条会话
装 Agent 估上下文时报「没有可用端点」，`?` 一路冒到顶、连接直接断掉。

这里验四件事：

1. 用 `/session` 切回钉着失效模型的会话：切得回去，底栏是全局池那个模型，失效的覆盖
   被清掉（不清的话每轮重踩一次、`/models` 里还显示着它），不提示（用户 09-24 定）；
2. 清掉之后在那条会话里照样跑得完一轮；
3. 启动时落到的会话钉着失效模型：屏上有一行说明、覆盖被清掉，再开一次不再提示；
4. 一半失效时只筛掉失效那条，覆盖留着。

09-20 起 `miyu` 启动开的是新会话（05e57350）：车道指针指着的会话有内容就另开一条，
空的才接着用。所以第 1、2、4 条要先 `/session` 切回钉了模型的那条；第 3 条只有
「指针指着一条空会话、它钉着失效模型」才走得到，就这么摆出来。

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
# 会话名是按第一条回复起的，桩模型每次回同一句，几条会话会重名：先改成独一份的名字，
# 之后 `/session 名字` 才切得准。
NAME = "钉模型走查"
REPLY = "走查的回复"


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


def completed_turns(session_id):
    with sqlite3.connect(f"file:{conversation_db()}?mode=ro", uri=True) as db:
        return db.execute(
            "SELECT COUNT(*) FROM turns WHERE session_id = ? AND status = 'completed'",
            (session_id,),
        ).fetchone()[0]


def wait_named(master, sink, name, timeout=10.0):
    """等 `/rename` 落库。屏上等名字不行：输入框里敲的就是它。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        with sqlite3.connect(f"file:{conversation_db()}?mode=ro", uri=True) as db:
            if db.execute("SELECT 1 FROM sessions WHERE name = ?", (name,)).fetchone():
                return True
        h.drain(master, 0.3, sink)
    return False


def type_line(master, sink, text):
    """敲一行再回车。回车要等输入框里真出现这行字再发：切回来的会话回放里已经有同样
    的字，只等「屏上有」会立刻放行，字和回车挤进同一次读，TUI 把回车当成粘贴里的
    换行，这一轮就没发出去（09-24 撞到一次）。所以数行数：比敲之前多一行才算。"""
    before = sum(text in line for line in h.render(bytes(sink)))
    os.write(master, text.encode())
    deadline = time.time() + 5.0
    while time.time() < deadline:
        h.drain(master, 0.1, sink)
        if sum(text in line for line in h.render(bytes(sink))) > before:
            break
    os.write(master, b"\r")


def wait_completed(master, sink, session_id, count, timeout=40.0):
    """等这条会话跑完的轮数到 `count`。屏上等回复的字不行：切回来时回放里就有。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if completed_turns(session_id) >= count:
            return True
        h.drain(master, 0.3, sink)
    return False


def run_tui(tag, act=None):
    """起一个 TUI，等底栏画出来，交给 `act(master, sink)` 做事，返回还原出来的屏幕与原始字节。"""
    tui, master = h.spawn_tui()
    sink = bytearray()
    # 换了 TUI 进程：虚拟屏从零开始，否则看到的是上一个进程的残影（run.py `render`）。
    h.reset_view()
    try:
        # 等底栏真的画出来再敲：新进程头一秒还没进 raw 模式，敲进去的回车会被
        # 行规程吃掉（run.py item24）。
        h.drain_until(master, sink, "stub-model", 20.0)
        h.settle(master, sink, quiet=0.5, timeout=5.0)
        if act:
            act(master, sink)
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
    screen = h.render(raw)
    (OUT / f"{tag}.txt").write_text("\n".join(screen), encoding="utf-8")
    (OUT / f"{tag}.raw").write_bytes(raw)
    return screen, raw


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

        # ── 0. 先跑一轮，把会话建出来、改个独一份的名字 ─────────────────
        def first_run(master, sink):
            type_line(master, sink, PROMPT)
            h.drain_until(master, sink, REPLY, 40.0)
            h.settle(master, sink, quiet=0.8, timeout=10.0)
            type_line(master, sink, f"/rename {NAME}")
            wait_named(master, sink, NAME)

        screen, _ = run_tui("00-first-run", first_run)
        report["起头能正常跑一轮"] = any(REPLY in line for line in screen)
        session_id = repl_lane_session()

        def switch_back(master, sink):
            type_line(master, sink, f"/session {NAME}")
            h.drain_until(master, sink, REPLY, 20.0)

        def switch_back_and_run(master, sink):
            done = completed_turns(session_id)
            switch_back(master, sink)
            type_line(master, sink, PROMPT)
            wait_completed(master, sink, session_id, done + 1)

        # ── 1. 给它钉一个供应商已经没有的模型，再切回去 ──────────────────
        pin(session_id, [{"provider_id": "stub", "model": GHOST}])
        screen, raw = run_tui("01-stale-pin", switch_back)
        text = "\n".join(screen)
        report["钉了失效模型也切得回去"] = "invalid admin response" not in raw.decode(
            "utf-8", "replace"
        ) and any(REPLY in line for line in screen)
        report["底栏是全局池那个模型"] = "stub-model" in text
        report["失效的覆盖被清掉了"] = pinned(session_id) is None
        report["切回时不提示"] = GHOST not in text

        # ── 2. 清掉之后在那条会话里照样跑得完一轮（回合路也退回了全局池）──
        done = completed_turns(session_id)
        run_tui("02-after-clear", switch_back_and_run)
        report["清掉后照样跑得完一轮"] = completed_turns(session_id) > done

        # ── 3. 启动时落到的会话钉着失效模型：说一声、清掉，再开不再提示 ──
        # 指针指着有内容的会话，启动就另开一条空的；空的这条下次启动还接着用。
        run_tui("03a-fresh")
        fresh_id = repl_lane_session()
        report["启动另开了一条空会话"] = fresh_id != session_id
        pin(fresh_id, [{"provider_id": "stub", "model": GHOST}])

        def say_once(master, sink):
            type_line(master, sink, PROMPT)
            wait_completed(master, sink, fresh_id, 1)

        screen, raw = run_tui("03b-stale-at-start", say_once)
        text = "\n".join(screen)
        report["启动时钉了失效模型也进得去"] = "invalid admin response" not in raw.decode(
            "utf-8", "replace"
        )
        report["启动时屏上说清了是哪一条失效"] = GHOST in text and "退回全局模型池" in text
        report["启动时失效的覆盖被清掉了"] = pinned(fresh_id) is None
        screen, _ = run_tui("03c-restart")
        report["清掉后再开不再提示"] = GHOST not in "\n".join(screen)

        # ── 4. 一半失效：剩下能用的那条照钉，不清、不提示 ────────────────
        pin(session_id, [
            {"provider_id": "stub", "model": GHOST},
            {"provider_id": "stub", "model": "stub-model"},
        ])
        done = completed_turns(session_id)
        screen, _ = run_tui("04-partly-stale", switch_back_and_run)
        report["一半失效时不清覆盖"] = pinned(session_id) is not None
        report["一半失效时不提示"] = GHOST not in "\n".join(screen)
        # 回合路（`turns/task.rs`）套的是同一道守卫：钉着一条失效的也得跑得完。
        report["一半失效时回合照跑"] = completed_turns(session_id) > done
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

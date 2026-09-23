#!/usr/bin/env python3
"""跨会话消息的全屏 TUI 走查(09-23):沙箱 daemon + stub.py + 真 PTY + pyte 抓屏。

TUI 开着「收件」(B,在线登记靠 TUI 的轮询自己报);「写代码」(A)用一次性 `miyu ask`
驱动,测具经 IPC 替它报在线(B 往回发时它得算开着)。判据:

    tui01_wake_block            A 发来 14 行 → B 的 TUI 画出「从 写代码（<短 id>）收到消息」那一块:
                                竖线串着 9 行 + 「⋮ 已省略」(和命令预览同一个规矩),不露外壳
    tui02_wake_reply            B 的 AI 接着回话(GOT: …)
    tui03_block_expands         点那一块的抬头,展开看到第 14 行
    tui04_replay_block          关掉 TUI 再开、切回 B:回放里还是那一块,不是用户气泡、不露外壳
    tui05_send_step             B 里让 AI 发给 A:时间线那一步写「给其他会话发送消息」,右边是
                                「<A 的短 id> 写代码」,底下露正文前几行(暗色)
    tui06_send_delivered        A 真收到了(库里 A 多了一轮跨会话消息)
    tui07_midturn_block         B 正跑着一轮(sleep 6)时 A 发来一句:这一轮里画出同样的那一块,
                                不画成用户气泡、不挂「排队中」,B 的 AI 在这一轮里读到(SAW_MIDTURN)

    python3 testkit/cross-session/tui.py <miyu 二进制>

产物(抓屏文本)在 /tmp/miyu-xs-tui,全过就删;不碰线上 8300。
"""

import json
import os
import shutil
import socket
import sqlite3
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


if len(sys.argv) != 2:
    print(__doc__)
    raise SystemExit(2)
BINARY = Path(sys.argv[1]).resolve()
OUT = Path("/tmp/miyu-xs-tui")
# run.py 在导入时读这几个环境变量定路径与端口:全指到 /tmp,别落进家目录。
os.environ.update(
    MIYU_BIN=str(BINARY),
    MIYU_HOME=str(OUT / "home"),
    MIYU_TUI_RUNTIME="/tmp/mxs-tui",
    MIYU_TUI_PORT=str(free_port()),
    STUB_PORT=str(free_port()),
    OUT=str(OUT),
)
for key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[key]
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "tui"))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

MESSAGE = "\n".join(f"第 {index} 行：构建好了" for index in range(1, 15))
REPLY = "回话正文" + "这一段够长会折成好几行，预览只露前面几行。" * 30


def write_config():
    (h.HOME / "config").mkdir(parents=True, exist_ok=True)
    config = {
        "config_version": 3,
        "oobe_done": True,
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{
            "id": "stub", "display_name": "Stub", "enabled": True,
            "base_url": f"http://127.0.0.1:{h.STUB_PORT}/v1", "protocol": "openai-chat",
            "api_key": "stub", "models": ["stub-model"], "default_model": "stub-model",
        }],
        "memory": {"enabled": False},
        "voice": {"enabled": False},
        # 这一步跑完不收成 `Worked for …`,好直接看它长什么样(收段是通用行为)。
        "display": {"fold_timeline": False},
    }
    (h.HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False), "utf-8")
    (h.HOME / "state").mkdir(parents=True, exist_ok=True)
    (h.HOME / "state" / "daemon-launch.json").write_text(json.dumps({"port": h.PORT}), "utf-8")


def ask(session, text, create=False):
    args = [str(BINARY), "ask", "--output-format", "json", "--session", session]
    if create:
        args.append("--create")
    env = {k: v for k, v in h.ENV.items() if k != "MIYU_TUI"}
    return subprocess.run([*args, text], env=env, stdin=subprocess.DEVNULL, cwd=str(h.HOME),
                          capture_output=True, text=True, timeout=120)


def query(sql, params=()):
    rows = []
    for db in sorted(h.HOME.rglob("conversation.db")):
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
        con.row_factory = sqlite3.Row
        rows.extend(dict(row) for row in con.execute(sql, params).fetchall())
        con.close()
    return rows


def ipc(command):
    sock_path = next(Path(h.RUNTIME).rglob("*.sock"))
    payload = json.dumps({"version": 3, **command}).encode()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(10)
        sock.connect(str(sock_path))
        sock.sendall(struct.pack(">I", len(payload)) + payload)
        (length,) = struct.unpack(">I", sock.recv(4))
        data = b""
        while len(data) < length:
            data += sock.recv(length - len(data))
        return json.loads(data)


def save(name, screen):
    (OUT / f"{name}.txt").write_text("\n".join(screen or []) + "\n", encoding="utf-8")


def main():
    report = {}
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    Path(h.RUNTIME).mkdir(exist_ok=True)
    write_config()
    h.kill_stale_daemon()
    stub = subprocess.Popen([sys.executable, str(HERE / "stub.py")],
                            env=dict(os.environ, STUB_PORT=str(h.STUB_PORT), STUB_LOG=str(OUT / "stub.jsonl")),
                            stdout=subprocess.DEVNULL, stderr=(OUT / "stub.err").open("w"))
    daemon = subprocess.Popen([str(BINARY), "__daemon", "--port", str(h.PORT)], env=h.ENV, cwd=str(h.HOME),
                              stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
    tui = tui2 = None
    keep_open = threading.Event()
    try:
        assert h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"), "stub not up"
        assert h.wait_http(f"{h.BASE}/api/health"), "daemon not up"
        tui, master = h.spawn_tui()
        sink = bytearray()
        h.drain_until(master, sink, "A G E N T", 20.0)
        h.drain(master, 1.0, sink)
        os.write(master, "hello from B".encode())
        h.drain_until(master, sink, "hello from B", 5.0)
        os.write(master, b"\r")
        r.wait_screen(master, sink, lambda s: any("OK" in line for line in s), 30.0)
        rows = query("SELECT session_id FROM turns WHERE user_content LIKE '%hello from B%'")
        b = rows[0]["session_id"]
        # 自动起名会让桩模型把它叫成「OK」;起个认得出的名字,重开之后 `/session 收件` 切回来。
        ipc({"command": "rename_session", "target": {"kind": "id", "id": b}, "name": "收件"})
        created = ask("写代码", "hello", create=True)
        assert created.returncode == 0, created.stderr
        a = query("SELECT session_id FROM sessions WHERE name = '写代码' AND kind = 'user'")[0]["session_id"]
        keep_open.set()

        def report_a():
            while keep_open.is_set():
                try:
                    ipc({"command": "presence", "viewer": "tk-a", "session": a})
                except OSError:
                    pass
                time.sleep(3)

        threading.Thread(target=report_a, daemon=True).start()
        time.sleep(2.0)

        sent = ask("写代码", f"TK send {b} {MESSAGE}")
        (OUT / "a-send.json").write_text(sent.stdout + sent.stderr, encoding="utf-8")
        screen = r.wait_screen(master, sink, lambda s: any("GOT:" in line for line in s), 40.0)
        screen = screen or r.LAST["screen"] or []
        save("b-wake", screen)
        head_row = next((i for i, line in enumerate(screen) if f"从 写代码（{a.rsplit('_', 1)[-1]}）收到消息" in line), None)
        body = screen[head_row + 1: head_row + 12] if head_row is not None else []
        report["tui01_wake_block"] = (
            head_row is not None
            and any("│ 第 1 行" in line for line in body)
            and any("│ 第 9 行" in line for line in body)
            and any("⋮" in line for line in body)
            and not any("第 14 行" in line for line in screen)
            and not any("<cross-session-message" in line for line in screen)
        )
        report["tui02_wake_reply"] = any("GOT:" in line for line in screen)
        if head_row is not None:
            column = screen[head_row].index("从")
            h.click(master, sink, column, head_row)
            screen = r.wait_screen(master, sink, lambda s: any("第 14 行" in line for line in s), 8.0)
            save("b-expanded", screen or r.LAST["screen"])
            report["tui03_block_expands"] = screen is not None
        else:
            report["tui03_block_expands"] = False

        # 关掉再开(开出来是新会话),`/session 收件` 切回 B,看回放。
        os.close(master)
        r.stop(tui)
        tui = None
        tui2, master2 = h.spawn_tui()
        sink2 = bytearray()
        h.drain_until(master2, sink2, "A G E N T", 20.0)
        h.drain(master2, 1.0, sink2)
        os.write(master2, "\x15/session 收件".encode())
        h.drain(master2, 0.6, sink2)
        os.write(master2, b"\r")
        screen = r.wait_screen(master2, sink2, lambda s: any("从 写代码（" in line for line in s), 20.0)
        save("b-replay", screen or r.LAST["screen"])
        report["tui04_replay_block"] = bool(screen) and not any(
            "<cross-session-message" in line for line in screen
        ) and any("│ 第 1 行" in line for line in screen)

        before_a = len(query("SELECT turn_id FROM turns WHERE session_id = ?", (a,)))
        message = f"TK send {a} {REPLY}"
        os.write(master2, message.encode())
        h.drain(master2, 1.0, sink2)
        os.write(master2, b"\r")
        screen = r.wait_screen(master2, sink2, lambda s: any("SENT:" in line for line in s), 40.0)
        screen = screen or r.LAST["screen"] or []
        save("b-send", screen)
        step_row = next((i for i, line in enumerate(screen) if "给其他会话发送消息" in line), None)
        report["tui05_send_step"] = (
            step_row is not None
            and f"{a.rsplit('_', 1)[-1]} 写代码" in screen[step_row]
            and any("│ 回话正文" in line for line in screen[step_row + 1: step_row + 12])
        )
        delivered = [row for row in query("SELECT user_content FROM turns WHERE session_id = ?", (a,))[before_a:]
                     if row["user_content"].startswith("<cross-session-message")]
        report["tui06_send_delivered"] = bool(delivered)

        # 插进正在跑的那一轮:走的是 queue.consumed 那条渲染路,和唤醒轮不是一条。
        os.write(master2, b"TK slow")
        h.drain(master2, 0.6, sink2)
        os.write(master2, b"\r")
        r.wait_screen(master2, sink2, lambda s: any("sleep 6" in line for line in s), 20.0)
        time.sleep(1.0)
        midturn = ask("写代码", f"TK send {b} MIDTURN 插一句")
        (OUT / "a-midturn.json").write_text(midturn.stdout + midturn.stderr, encoding="utf-8")
        screen = r.wait_screen(master2, sink2, lambda s: any("SAW_MIDTURN" in line for line in s), 40.0)
        screen = screen or r.LAST["screen"] or []
        save("b-midturn", screen)
        blocks = [i for i, line in enumerate(screen) if "从 写代码（" in line]
        last_block = blocks[-1] if blocks else None
        report["tui07_midturn_block"] = (
            last_block is not None
            and any("│ MIDTURN 插一句" in line for line in screen[last_block + 1: last_block + 3])
            and any("SAW_MIDTURN" in line for line in screen)
            and not any("<cross-session-message" in line for line in screen)
            and not any("排队中" in line for line in screen)
        )
    except Exception as error:  # noqa: BLE001 — 走查中途出错也要记成失败
        report["aborted"] = False
        print("aborted:", repr(error))
    finally:
        keep_open.clear()
        r.stop(tui, tui2, daemon, stub)
    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"{passed}/{len(report)} passed  抓屏在 {OUT}")
    if report and passed == len(report):
        shutil.rmtree(OUT, ignore_errors=True)
    shutil.rmtree(h.RUNTIME, ignore_errors=True)
    return 0 if report and passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())

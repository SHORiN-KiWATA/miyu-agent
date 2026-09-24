#!/usr/bin/env python3
"""断点续跑的全屏 TUI 走查(09-24):沙箱 daemon + stub.py + 真 PTY + pyte 抓屏。

TUI 里起一轮慢工具(sleep 30),命令跑起来之后把 daemon SIGKILL 掉,再在外面起一个新的
(相当于用户 `miyu daemon restart` / 换二进制后重启)。判据:

    tui01_restart_notice    TUI 自己挂到续跑那一轮上,画出「重启了，接着上一轮继续」那一行
    tui02_resumed_reply     续跑那一轮的回话(RESUMED attempt=1)出现在同一屏
    tui03_no_envelope       屏上不露 <service-restart 外壳
    tui04_replay_notice     关掉 TUI 再开、切回那条会话:回放里还是那一行提示,不是用户气泡

    python3 testkit/turn-resume/tui.py <miyu 二进制>

产物(抓屏文本)在 /tmp/miyu-resume-tui,全过就删(KEEP=1 留着看版式);不碰线上 8300。
"""

import json
import os
import shutil
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
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
OUT = Path("/tmp/miyu-resume-tui")
os.environ.update(
    MIYU_BIN=str(BINARY),
    MIYU_HOME=str(OUT / "home"),
    MIYU_TUI_RUNTIME="/tmp/mrs-tui",
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

NOTICE = "重启了，接着上一轮继续"
# 09-24 起提示里不带「Miyu」（用户：去掉这里的 Miyu）。只查 NOTICE 的话老文案也含它。
STALE = "Miyu 重启"


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
    }
    (h.HOME / "config" / "config.jsonc").write_text(json.dumps(config, ensure_ascii=False), "utf-8")
    (h.HOME / "state").mkdir(parents=True, exist_ok=True)
    (h.HOME / "state" / "daemon-launch.json").write_text(json.dumps({"port": h.PORT}), "utf-8")


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


def start_daemon(boot):
    return subprocess.Popen([str(BINARY), "__daemon", "--port", str(h.PORT)], env=h.ENV, cwd=str(h.HOME),
                            stdout=(OUT / f"daemon-{boot}.log").open("w"), stderr=subprocess.STDOUT)


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
    daemon = start_daemon(1)
    tui = tui2 = None
    try:
        assert h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"), "stub not up"
        assert h.wait_http(f"{h.BASE}/api/health"), "daemon not up"
        tui, master = h.spawn_tui()
        sink = bytearray()
        h.drain_until(master, sink, "A G E N T", 20.0)
        h.drain(master, 1.0, sink)
        os.write(master, "TK slow T1".encode())
        h.drain_until(master, sink, "TK slow T1", 5.0)
        os.write(master, b"\r")
        screen = r.wait_screen(master, sink, lambda s: any("sleep 30" in line for line in s), 30.0)
        save("before-kill", screen or r.LAST["screen"])
        rows = query("SELECT session_id FROM turns WHERE user_content LIKE '%TK slow T1%'")
        session = rows[0]["session_id"]
        ipc({"command": "rename_session", "target": {"kind": "id", "id": session}, "name": "续跑"})
        time.sleep(1.5)

        daemon.send_signal(signal.SIGKILL)
        daemon.wait(timeout=20)
        h.drain(master, 2.0, sink)
        save("after-kill", r.LAST.get("screen"))
        daemon = start_daemon(2)
        assert h.wait_http(f"{h.BASE}/api/health"), "second daemon not up"

        screen = r.wait_screen(master, sink, lambda s: any("RESUMED attempt=1" in line for line in s), 60.0)
        screen = screen or r.LAST["screen"] or []
        save("resumed", screen)
        if os.environ.get("KEEP"):
            (OUT / "resumed.raw").write_bytes(bytes(sink))
        report["tui01_restart_notice"] = any(NOTICE in line and STALE not in line for line in screen)
        report["tui02_resumed_reply"] = any("RESUMED attempt=1" in line for line in screen)
        report["tui03_no_envelope"] = not any("<service-restart" in line for line in screen)

        os.close(master)
        r.stop(tui)
        tui = None
        tui2, master2 = h.spawn_tui()
        sink2 = bytearray()
        h.drain_until(master2, sink2, "A G E N T", 20.0)
        h.drain(master2, 1.0, sink2)
        os.write(master2, "\x15/session 续跑".encode())
        h.drain(master2, 0.6, sink2)
        os.write(master2, b"\r")
        screen = r.wait_screen(master2, sink2, lambda s: any("RESUMED attempt=1" in line for line in s), 20.0)
        screen = screen or r.LAST["screen"] or []
        save("replay", screen)
        report["tui04_replay_notice"] = (
            any(NOTICE in line for line in screen)
            and not any("<service-restart" in line for line in screen)
        )
    except Exception as error:  # noqa: BLE001 — 走查中途出错也要记成失败
        report["aborted"] = False
        print("aborted:", repr(error))
    finally:
        r.stop(tui, tui2, daemon, stub)
    passed = sum(1 for value in report.values() if value)
    for name, value in report.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"{passed}/{len(report)} passed  抓屏在 {OUT}")
    if report and passed == len(report) and not os.environ.get("KEEP"):
        shutil.rmtree(OUT, ignore_errors=True)
    shutil.rmtree(h.RUNTIME, ignore_errors=True)
    return 0 if report and passed == len(report) else 1


if __name__ == "__main__":
    sys.exit(main())

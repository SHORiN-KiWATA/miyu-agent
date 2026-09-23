#!/usr/bin/env python3
"""「终端集成会话默认模式」黑盒(09-18):隔离 home + 独立端口 daemon + 桩 LLM。

config.terminal_session_mode(normal|dev,默认 normal)决定终端集成车道——shell
提示符敲的话(shellhook)落进去的那条会话,也就是 daemon 的 current_session 指针
指着的会话——跑普通还是开发模式。daemon 启动和 `miyu reload` 各对一次:掰「终端
集成会话」(id default)的人格 + 把指针指过去。验:

    默认 → 指针在终端集成会话上 mode=normal,shellhook 回合工具面是普通面
    指针停在另一条普通会话上(模拟换人格留下的状态)+ 改 dev + miyu reload
        → 指针拉回终端集成会话、mode=dev,shellhook 回合工具面换成 dev 面(不用重启)
    改回 normal + reload → 回到普通;别的会话不受影响;阅后即焚的裸 ask 不受影响
    daemon 带 dev 配置重启(指针又被拨到普通会话上)→ 启动路也把指针拉回来
    再重启一次 → dev 人格的终端会话不会被当「不可用」撵走(第一版的坑)
    非法值 → daemon 拒启

用法:先 `cargo build`,再 `python3 testkit/cli/terminal_mode.py`(BIN= 换二进制)。
"""
import importlib.util
import json
import os
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BASE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("clitk", BASE / "run.py")
tk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tk)

CONFIG = tk.HOME / "config" / "config.jsonc"
STUB_LOG = tk.OUT / "stub.jsonl"


def set_mode(value):
    cfg = json.loads(CONFIG.read_text(encoding="utf-8"))
    if value is None:
        cfg.pop("terminal_session_mode", None)
    else:
        cfg["terminal_session_mode"] = value
    CONFIG.write_text(json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8")


def start_daemon():
    daemon = subprocess.Popen([str(tk.MIYU), "daemon", "--port", str(tk.PORT)], env=tk.env(),
                              stdout=(tk.OUT / "daemon.log").open("a"), stderr=subprocess.STDOUT)
    for _ in range(60):
        if tk.find_socket():
            break
        time.sleep(0.5)
    assert tk.find_socket(), "daemon socket never appeared"
    time.sleep(1.5)
    return daemon


def stop_daemon(daemon):
    subprocess.run([str(tk.MIYU), "daemon", "stop"], env=tk.env(), capture_output=True, timeout=30)
    try:
        daemon.wait(timeout=10)
    except subprocess.TimeoutExpired:
        daemon.kill()
    for p in tk.RUN.rglob("*.sock"):
        p.unlink(missing_ok=True)


def sessions():
    code, out, err = tk.cli(["session", "list", "--json"])
    assert code == 0, err
    return json.loads(out).get("sessions", [])


def lane_entry():
    """终端集成车道 = is_current 那条。"""
    for entry in sessions():
        if entry.get("is_current"):
            return entry
    raise AssertionError("no current session")


def conversation_db():
    for path in tk.HOME.rglob("conversation.db"):
        con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        try:
            if con.execute("select count(*) from sessions where session_id='default'").fetchone()[0]:
                return path
        finally:
            con.close()
    raise AssertionError("conversation.db with the terminal session not found")


def park_pointer_on(session_id):
    """daemon 停着的时候把终端车道指针拨到别的会话上(模拟换人格留下的状态)。"""
    con = sqlite3.connect(conversation_db())
    try:
        cols = [row[1] for row in con.execute("pragma table_info(app_state)")]
        key_col, val_col = cols[0], cols[1]
        con.execute(f"update app_state set {val_col}=? where {key_col}='current_session'", (session_id,))
        con.commit()
    finally:
        con.close()


def shellhook_tools(tag):
    """shell 提示符敲一句(shellhook 形态),回桩 LLM 看到的工具面。"""
    before = STUB_LOG.read_text(encoding="utf-8").count("\n") if STUB_LOG.exists() else 0
    code, out, err = tk.cli(["--shell-intercept", "--shell", "fish", f"TK {tag}"], timeout=120)
    assert code == 0, f"code={code} err={err[-300:]}"
    lines = STUB_LOG.read_text(encoding="utf-8").splitlines()[before:]
    assert lines, "stub saw no request"
    return sorted(json.loads(lines[-1])["summary"].get("tools") or [])


def ask_tools():
    """程序驱动的 `miyu ask --output-format json`(阅后即焚,没传 --mode):显式接口,不跟配置。"""
    code, out, err = tk.cli(["ask", "--output-format", "json", "TK ask"])
    assert code == 0, err
    return sorted(tk.reply_summary(tk.json_lines(out)[0]).get("tools") or [])


def plain_tools(tag):
    """裸 `miyu "…"`(阅后即焚,终端那条路):按配置建 dev/普通会话。"""
    before = STUB_LOG.read_text(encoding="utf-8").count("\n") if STUB_LOG.exists() else 0
    code, out, err = tk.cli([f"TK {tag}"], timeout=120)
    assert code == 0, f"code={code} err={err[-300:]}"
    lines = STUB_LOG.read_text(encoding="utf-8").splitlines()[before:]
    assert lines, "stub saw no request"
    return sorted(json.loads(lines[-1])["summary"].get("tools") or [])


def main():
    assert tk.MIYU.exists(), f"missing binary {tk.MIYU}; run cargo build"
    tk.build_home()
    # build_home 借的是真机配置,里面可能已经写着 terminal_session_mode:先抹掉,从默认值起。
    set_mode(None)
    stub = subprocess.Popen([sys.executable, str(BASE / "stub_llm.py")],
                            env=dict(os.environ, STUB_PORT=str(tk.STUB_PORT), STUB_LOG=str(STUB_LOG)),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    daemon = start_daemon()
    check = tk.check
    try:
        lane = lane_entry()
        check("默认:车道指针在终端集成会话上,mode=normal",
              lane.get("session_id") == "default" and lane.get("mode") == "normal", lane)
        normal_tools = shellhook_tools("one")
        check("默认:shellhook 回合有普通工具面", bool(normal_tools), len(normal_tools))
        lane = lane_entry()
        check("shellhook 回合落在终端集成会话里", lane.get("session_id") == "default"
              and lane.get("last_user_content") == "TK one", lane)

        # 指针停在另一条普通会话上(换人格会留下这种状态),再改 dev + reload
        code, out, err = tk.cli(["ask", "--output-format", "json", "--session", "side", "--create", "TK side"])
        check("另建一条普通会话 side", code == 0, err.strip()[:120])
        side = next(e for e in sessions() if e.get("name") == "side")
        stop_daemon(daemon)
        park_pointer_on(side["session_id"])
        daemon = start_daemon()
        lane = lane_entry()
        check("(夹具)指针拨到了 side 上", lane.get("session_id") == side["session_id"], lane.get("name"))

        set_mode("dev")
        code, _, err = tk.cli(["reload"])
        check("miyu reload 成功", code == 0, err.strip()[:120])
        lane = lane_entry()
        check("改 dev + reload:指针拉回终端集成会话,mode=dev(不用重启)",
              lane.get("session_id") == "default" and lane.get("mode") == "dev", lane)
        dev_tools = shellhook_tools("two")
        check("改 dev 后:shellhook 回合工具面换成 dev 面", dev_tools and dev_tools != normal_tools,
              f"normal={len(normal_tools)} dev={len(dev_tools)}")
        lane = lane_entry()
        check("那一轮落在终端集成会话里", lane.get("session_id") == "default"
              and lane.get("last_user_content") == "TK two", lane.get("last_user_content"))
        side = next(e for e in sessions() if e.get("name") == "side")
        check("side 不受影响(mode=normal)", side.get("mode") == "normal", side)
        # 裸 `miyu "…"`(阅后即焚)也是终端那条路:客户端按配置建 dev 会话,跟着走;
        # 程序驱动的 `ask --output-format json` 有自己的 --mode,没传就是普通,不跟配置。
        plain_dev = plain_tools("plain-dev")
        check("裸 miyu \"…\" 单次也跟着 dev(工具面 = dev 面)", plain_dev == dev_tools,
              f"plain={len(plain_dev)} dev={len(dev_tools)}")
        ask_dev = ask_tools()
        check("程序驱动 ask --output-format json 没传 --mode 仍是普通面", ask_dev == normal_tools,
              f"ask={len(ask_dev)} normal={len(normal_tools)}")

        set_mode("normal")
        code, _, err = tk.cli(["reload"])
        lane = lane_entry()
        check("改回 normal + reload:终端集成会话 mode=normal", code == 0
              and lane.get("session_id") == "default" and lane.get("mode") == "normal", lane)
        check("改回 normal 后 shellhook 工具面恢复普通面", shellhook_tools("three") == normal_tools)
        plain_normal = plain_tools("plain-normal")
        check("改回 normal 后裸 miyu \"…\" 也是普通面", plain_normal == normal_tools,
              f"plain={len(plain_normal)} normal={len(normal_tools)}")

        # 启动那条路:指针又停在普通会话上、配置是 dev
        stop_daemon(daemon)
        park_pointer_on(side["session_id"])
        set_mode("dev")
        daemon = start_daemon()
        lane = lane_entry()
        check("daemon 带 dev 配置启动:指针拉回终端集成会话,mode=dev",
              lane.get("session_id") == "default" and lane.get("mode") == "dev", lane)
        check("启动路的 shellhook 回合也是 dev 面", shellhook_tools("four") == dev_tools)
        # 再重启一次:dev 人格的终端会话不能被当「不可用」撵走
        stop_daemon(daemon)
        daemon = start_daemon()
        lane = lane_entry()
        check("再重启:指针仍在终端集成会话上,mode=dev",
              lane.get("session_id") == "default" and lane.get("mode") == "dev", lane)
        names = [e.get("name") for e in sessions()]
        check("没有多冒出自举的空会话", len(names) == 2, names)

        stop_daemon(daemon)
        set_mode("bogus")
        daemon = subprocess.Popen([str(tk.MIYU), "daemon", "--port", str(tk.PORT)], env=tk.env(),
                                  stdout=(tk.OUT / "daemon.log").open("a"), stderr=subprocess.STDOUT)
        time.sleep(3)
        code, out, err = tk.cli(["session", "list", "--json"])
        check("非法值 bogus:daemon 拒启/命令报错(退出码非 0)", code != 0 or daemon.poll() is not None,
              f"code={code} daemon={daemon.poll()} err={err.strip()[:120]}")
    finally:
        stop_daemon(daemon)
        stub.terminate()
        (tk.OUT / "terminal-mode-verdict.json").write_text(json.dumps(tk.results, ensure_ascii=False, indent=2), encoding="utf-8")
    passed = sum(1 for r in tk.results if r["ok"])
    print(f"\n{passed}/{len(tk.results)} passed")
    sys.exit(0 if passed == len(tk.results) else 1)


if __name__ == "__main__":
    main()

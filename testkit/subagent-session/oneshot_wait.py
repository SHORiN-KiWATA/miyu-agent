#!/usr/bin/env python3
"""一次性命令等子代理(09-26 子代理只在后台跑):隔离 home + 独立端口 daemon + 桩 LLM(stub.py)。

子代理派出去,派它的那一轮当场收尾;结论在子代理跑完、报告叫醒会话再起的那一轮里。
一次性命令要等整棵子树收尾、带着结论退出。判据:

    text_waits_for_the_report        `miyu "…"` 输出里先有主回合、再有报告叫醒的那一轮(带抬头与结论),退出码 0
    ephemeral_session_cleaned        退出后阅后即焚的会话删掉了,没有留下在跑的任务
    json_done_carries_the_conclusion `--output-format json` 的终态正文是报告叫醒那一轮的
    stream_json_one_done_at_the_end  stream-json:两个 started、一条 waiting 提示、done 只有一条且在最后
    pty_follow_skips_writeback       在真终端里等的时候,daemon 不回写也不补「终端不在提示符」通知
    pty_spinner_says_what_it_waits_for  等的时候转轮旁写着还有几个子代理在跑
    ctrl_c_stops_the_subtree         等的时候按 Ctrl+C:退出码 130,子代理的命令被停,会话删掉

    cargo build
    python3 testkit/subagent-session/oneshot_wait.py        # BIN=<旧二进制> 可证明修之前是红的

绝不触碰线上 8300 daemon;产物在 ~/.cache/miyu-oneshot-wait/。
"""
import fcntl
import importlib.util
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time
from pathlib import Path

os.environ.setdefault("OUT", "~/.cache/miyu-oneshot-wait")
os.environ.setdefault("PORT", "18558")
os.environ.setdefault("STUB_PORT", "18557")

BASE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("sas", BASE / "run.py")
sas = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sas)
# 运行目录和 run.py 分开,两个走查先后跑互不踩。
sas.RUN = Path.home() / ".cache" / "miyu-osw-run"

ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\r")


def plain(text):
    return ANSI.sub("", text)


def one_shot_sessions():
    return sas.query("SELECT session_id FROM sessions WHERE kind = 'ask'")


def running_jobs():
    return [job for job in sas.jobs_overview() if job.get("running")]


def keep(name, text):
    """每条命令的原始输出留一份,红了好查。"""
    (sas.OUT / f"{name}.out").write_text(text, "utf-8")


def daemon_log():
    """daemon 的 tracing 日志(stdout 那份只有启动横幅)。"""
    logs = sorted((sas.HOME / "cache" / "logs").glob("miyu.*.log"))
    return "\n".join(p.read_text(errors="replace") for p in logs)


def run_in_pty(args, interrupt_after=None, timeout=90):
    """在真终端里跑一条命令,收齐输出。`interrupt_after` 出现后往终端里敲 Ctrl+C。"""
    pid, fd = pty.fork()
    if pid == 0:
        # pty.fork 给的终端是 0×0:转轮旁的字会被裁成零宽,先给个正常尺寸。
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        env = sas.env()
        env["TERM"] = "xterm-256color"
        os.execve(str(sas.BIN), [str(sas.BIN), *args], env)
    output = b""
    interrupted = False
    deadline = time.time() + timeout
    status = None
    reaped = False
    while time.time() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                chunk = b""
            if not chunk:
                break
            output += chunk
            # 终端问光标位置(ESC[6n)就答一句,不然渲染器会干等。
            if b"\x1b[6n" in chunk:
                os.write(fd, b"\x1b[1;1R")
        text = output.decode(errors="replace")
        if interrupt_after and not interrupted and interrupt_after in plain(text):
            time.sleep(1.5)
            os.write(fd, b"\x03")
            interrupted = True
        done, polled = os.waitpid(pid, os.WNOHANG)
        if done:
            status, reaped = polled, True
            # 把残余输出读干净。
            while True:
                ready, _, _ = select.select([fd], [], [], 0.2)
                if not ready:
                    break
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
            break
    else:
        os.kill(pid, signal.SIGKILL)
    if not reaped:
        # 读到 EOF 就出了循环,子进程可能还没收尸:这里等它,退出码才是真的。
        _, status = os.waitpid(pid, 0)
    os.close(fd)
    code = os.waitstatus_to_exitcode(status) if status is not None else None
    return code, output.decode(errors="replace"), interrupted


def main():
    assert sas.BIN.exists(), f"missing binary {sas.BIN}"
    sas.build_home()
    stub_env = dict(os.environ, STUB_PORT=str(sas.STUB_PORT), STUB_LOG=str(sas.STUB_LOG))
    stub = subprocess.Popen([sys.executable, str(BASE / "stub.py")], env=stub_env)
    daemon = None
    try:
        time.sleep(0.5)
        daemon = sas.start_daemon()

        # 1. 文本输出:主回合 → 等 → 报告叫醒的那一轮。
        started = time.time()
        code, out, err = sas.cli(["TK nap"], timeout=120)
        seconds = round(time.time() - started, 1)
        keep("text", out + "\n--- stderr ---\n" + err)
        text = plain(out)
        main_at = text.find("PARENT_STARTED_BG")
        woke_at = text.find("WOKEN:")
        sas.check("text_waits_for_the_report",
                  code == 0 and 0 <= main_at < woke_at and "CHILD_RESULT ok" in text[woke_at:]
                  and "⚙" in text[main_at:woke_at],
                  {"code": code, "seconds": seconds, "tail": text[-300:], "err": err[-200:]})
        leftover = sas.wait_for(lambda: not one_shot_sessions() and not running_jobs(), 15)
        sas.check("ephemeral_session_cleaned", leftover,
                  {"sessions": len(one_shot_sessions()), "running": len(running_jobs())})

        # 2. JSON:终态正文是报告叫醒那一轮的。
        code, out, err = sas.cli(["ask", "--output-format", "json", "TK nap"], timeout=120)
        keep("json", out + "\n--- stderr ---\n" + err)
        lines = [json.loads(l) for l in out.splitlines() if l.strip().startswith("{")]
        final = lines[-1] if lines else {}
        sas.check("json_done_carries_the_conclusion",
                  code == 0 and len(lines) == 1 and final.get("type") == "done"
                  and final.get("text", "").startswith("WOKEN:") and "CHILD_RESULT ok" in final.get("text", ""),
                  {"code": code, "lines": len(lines), "text": final.get("text", "")[:120], "err": err[-200:]})

        # 3. stream-json:两轮的事件都吐出来,done 只有一条且在最后。
        code, out, err = sas.cli(["ask", "--output-format", "stream-json", "TK nap"], timeout=120)
        keep("stream", out + "\n--- stderr ---\n" + err)
        events = [json.loads(l) for l in out.splitlines() if l.strip().startswith("{")]
        types = [event.get("type") for event in events]
        notices = [event.get("message", "") for event in events if event.get("type") == "notice"]
        sas.check("stream_json_one_done_at_the_end",
                  code == 0 and types.count("done") == 1 and types[-1] == "done"
                  and types.count("started") >= 2 and any("waiting for" in n for n in notices)
                  and events[-1].get("text", "").startswith("WOKEN:"),
                  {"code": code, "types": types[:6] + ["…"] + types[-3:], "notices": notices[:2]})

        # 4. 真终端里等:daemon 认得出发起的命令还在前台,不回写、不补通知。
        log_before = len(daemon_log())
        code, out, _ = run_in_pty(["TK nap"], timeout=120)
        keep("pty", out)
        text = plain(out)
        log = daemon_log()[log_before:]
        sas.check("pty_follow_skips_writeback",
                  code == 0 and "WOKEN:" in text and "fell back to a notification" not in log
                  and "shown by the originating command" in log,
                  {"code": code, "woken": "WOKEN:" in text,
                   "fallback": "fell back to a notification" in log,
                   "skipped": "shown by the originating command" in log})
        sas.check("pty_spinner_says_what_it_waits_for", "等 1 个子代理跑完" in text,
                  {"tail": text[-160:]})

        # 5. 等的时候按 Ctrl+C:整棵子树停下,按取消退出,会话删掉。
        code, out, interrupted = run_in_pty(["TK fg-slow"], interrupt_after="PARENT_STARTED_BG", timeout=90)
        keep("ctrl_c", out)
        stopped = sas.wait_for(lambda: not sas.leftovers("sleep 40.5") and not running_jobs(), 15)
        cleaned = sas.wait_for(lambda: not one_shot_sessions(), 15)
        sas.check("ctrl_c_stops_the_subtree",
                  interrupted and code == 130 and stopped and cleaned,
                  {"interrupted": interrupted, "code": code, "sleep_left": sas.leftovers("sleep 40.5"),
                   "running": len(running_jobs()), "sessions": len(one_shot_sessions()),
                   "tail": plain(out)[-200:]})
    finally:
        if daemon is not None:
            sas.stop_daemon(daemon)
        stub.terminate()
        time.sleep(1.0)
        left = sas.leftovers("sleep 40.5")
        sas.check("no_orphans", not left, left)
        (sas.OUT / "verdict.json").write_text(json.dumps(sas.results, ensure_ascii=False, indent=2), "utf-8")
    passed = sum(sas.results.values())
    print(f"\n{passed}/{len(sas.results)} passed")
    sys.exit(0 if passed == len(sas.results) else 1)


if __name__ == "__main__":
    main()

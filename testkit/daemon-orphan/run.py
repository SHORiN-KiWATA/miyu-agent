#!/usr/bin/env python3
"""起 daemon 的人没了之后，daemon 该不该跟着走。

09-20 实录：用户后台攒了 19 个 `miyu __daemon` 僵进程，最老 4 天。全是测具的
沙箱 daemon——测具跑成 `timeout N python3 testkit/xxx.py`，超时的时候 SIGTERM
打在 python 上，而 python 默认的 SIGTERM 处理不跑 `finally`，脚本里那句
`daemon.terminate()` 永远没机会执行。

守两件事，缺一不可：

1. 测具那种（直接 spawn `__daemon`）：启动者一死，daemon 跟着退。
   连启动者被 SIGKILL 也得退——这正是 `finally` 救不了的那一种。
2. 真 daemon（`miyu daemon start` 那条路，带 detached 标记）：启动者退出之后
   照样活着。终端关了 daemon 就没了的话，整个后台服务模型就垮了。

跑法：

    cargo build
    python3 testkit/daemon-orphan/run.py --binary /绝对路径/target/debug/miyu
"""

import argparse
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def alive(pid):
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    # 过继给 init 之后仍然是 alive；僵尸态不算活着。
    try:
        state = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[0]
    except (OSError, IndexError):
        return False
    return state != "Z"


def wait_gone(pid, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not alive(pid):
            return True
        time.sleep(0.2)
    return not alive(pid)


def daemon_env(home, extra=None):
    env = dict(os.environ, MIYU_HOME=str(home))
    env.pop("MIYU_DAEMON_DETACHED", None)
    if extra:
        env.update(extra)
    return env


def spawn_via_launcher(binary, home, port, log):
    """测具那种：一个中间壳子直接 spawn `__daemon`，好让我们能杀掉「启动者」。"""
    code = (
        "import subprocess,sys,time\n"
        f"child = subprocess.Popen([{str(binary)!r}, '__daemon', '--port', '{port}'],"
        f" stdout=open({str(log)!r},'ab'), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)\n"
        "print(child.pid, flush=True)\n"
        "time.sleep(600)\n"
    )
    launcher = subprocess.Popen(
        [sys.executable, "-c", code],
        env=daemon_env(home),
        stdout=subprocess.PIPE,
        text=True,
    )
    daemon_pid = int(launcher.stdout.readline().strip())
    return launcher, daemon_pid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument(
        "--other-binary",
        type=Path,
        help="另一个 build id 的 miyu，用来验「换二进制重启不留旧进程」；不给就跳过那一项",
    )
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.exists():
        print(f"! 先 cargo build：{binary} 不存在", file=sys.stderr)
        return 2

    sandbox = sandbox_dir.make("miyu-orphan-", delete_at_exit=False)
    report = {}
    leaked = []
    try:
        # 1. 启动者被 SIGKILL（`finally` 绝对救不了的那一种）
        home = sandbox / "kill9"
        home.mkdir(parents=True)
        launcher, daemon_pid = spawn_via_launcher(
            binary, home, free_port(), sandbox / "kill9.log"
        )
        time.sleep(2.0)
        report["测具形态的 daemon 起得来"] = alive(daemon_pid)
        launcher.send_signal(signal.SIGKILL)
        launcher.wait(timeout=5)
        report["启动者被 SIGKILL 后 daemon 跟着退"] = wait_gone(daemon_pid, 15)
        if alive(daemon_pid):
            leaked.append(daemon_pid)

        # 2. 启动者正常退出
        home = sandbox / "exit"
        home.mkdir(parents=True)
        launcher, daemon_pid = spawn_via_launcher(
            binary, home, free_port(), sandbox / "exit.log"
        )
        time.sleep(2.0)
        launcher.terminate()
        launcher.wait(timeout=5)
        report["启动者正常退出后 daemon 也退"] = wait_gone(daemon_pid, 15)
        if alive(daemon_pid):
            leaked.append(daemon_pid)

        # 3. 真 daemon：`miyu daemon start` 会 setsid 并打 detached 标记，
        #    启动它的那条命令退出之后必须继续活着。
        home = sandbox / "real"
        home.mkdir(parents=True)
        port = free_port()
        start = subprocess.run(
            [str(binary), "daemon", "--port", str(port), "start"],
            env=daemon_env(home),
            capture_output=True,
            text=True,
            timeout=90,
            stdin=subprocess.DEVNULL,
        )
        time.sleep(3.0)
        found = subprocess.run(
            ["pgrep", "-f", f"__daemon --port {port}"],
            capture_output=True,
            text=True,
        ).stdout.split()
        real_pid = int(found[0]) if found else 0
        report["真 daemon 起得来"] = real_pid != 0 and alive(real_pid)
        if not report["真 daemon 起得来"]:
            print(f"  （daemon start 输出：{start.stdout.strip()} {start.stderr.strip()}）")
        # 启动它的 `daemon start` 命令早就退了（上面 run 已经返回）。
        time.sleep(3.0)
        report["真 daemon 在启动命令退出后仍然活着"] = real_pid != 0 and alive(real_pid)
        if real_pid:
            subprocess.run(
                # `--port` 只认 start / restart，stop 靠 MIYU_HOME 找自己那只。
                [str(binary), "daemon", "stop"],
                env=daemon_env(home),
                capture_output=True,
                timeout=60,
                stdin=subprocess.DEVNULL,
            )
            report["真 daemon 停得掉"] = wait_gone(real_pid, 20)
            if alive(real_pid):
                leaked.append(real_pid)
        # 4. 用户 09-20 报的那个形态：换一个二进制再起，旧的那只该被收掉。
        #    走的是 ensure_daemon 里 `build_id` 对不上就 restart_stale_daemon
        #    那条路——它发 IPC Shutdown 再等进程消失，等不到只会记个错然后
        #    照样起新的，于是旧的留在后台。
        if args.other_binary:
            other = args.other_binary.resolve()
            home = sandbox / "swap"
            home.mkdir(parents=True)
            port = free_port()
            subprocess.run(
                [str(other), "daemon", "--port", str(port), "start"],
                env=daemon_env(home), capture_output=True, timeout=90,
                stdin=subprocess.DEVNULL,
            )
            time.sleep(3.0)
            old = subprocess.run(
                ["pgrep", "-f", f"__daemon --port {port}"],
                capture_output=True, text=True,
            ).stdout.split()
            old_pid = int(old[0]) if old else 0
            report["旧二进制的 daemon 起得来"] = old_pid != 0 and alive(old_pid)
            # 换二进制起：build id 对不上，应当先收掉旧的
            subprocess.run(
                [str(binary), "daemon", "--port", str(port), "start"],
                env=daemon_env(home), capture_output=True, timeout=120,
                stdin=subprocess.DEVNULL,
            )
            report["换二进制之后旧 daemon 被收掉"] = wait_gone(old_pid, 30)
            survivors = subprocess.run(
                ["pgrep", "-f", f"__daemon --port {port}"],
                capture_output=True, text=True,
            ).stdout.split()
            report["同一个家目录只剩一只 daemon"] = len(survivors) == 1
            for pid in survivors:
                leaked.append(int(pid))
            if alive(old_pid):
                leaked.append(old_pid)
    finally:
        for pid in leaked:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        shutil.rmtree(sandbox, ignore_errors=True)

    passed = 0
    for name, ok in report.items():
        print(f"{'✅' if ok else '❌'} {name}")
        passed += bool(ok)
    print(f"\n{passed}/{len(report)} passed")
    return 0 if passed == len(report) else 1


if __name__ == "__main__":
    raise SystemExit(main())

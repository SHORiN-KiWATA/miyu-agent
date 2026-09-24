#!/usr/bin/env python3
"""一个家目录只能有一个 daemon —— 端到端验收。

复现的是 09-21 本机那个现场：同一个 `~/.miyu`，一个 daemon 从设了
`MIYU_HOME` 的 shell 起（runtime_dir 是 `miyu-<hash>`），另一个从没设的
shell 起（runtime_dir 是字面量 `miyu`），两把运行时锁互相看不见，于是两个
daemon 同时跑在同一份数据上，还各自拉起一个 miyu-voice 抢同一个麦克风。

用法：
    python3 testkit/daemon-singleton/run.py [--binary <path>]
"""

import argparse
import json
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

PASS, FAIL = [], []


def check(name, ok, detail=""):
    (PASS if ok else FAIL).append(name)
    print(f"  {'✓' if ok else '✗'} {name}" + (f"  {detail}" if detail else ""))


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def clean_env(home, runtime):
    """隔离一套环境。

    凭据一律抹掉，`XDG_*` 也一并改指隔离目录：这些用例只关心「锁放不放行」，
    不该有机会摸到开发机上的任何登录态（中转线那几家的凭据是 CLI 子进程自己
    从 `XDG_*` 底下读的，只改 `HOME` 拦不住）。

    抹干净之后 `miyu ask` 仍会发一次真请求并撞上 HTTP 429——那是**开箱默认**
    的 opencode Zen 匿名桶（`default_opencodezen()` 的 `api_key` 本来就是
    `None`），不花用户的额度。场景 6 要断的是「有没有被单例锁挡下」，模型这步
    因为什么停下都不影响判定，所以留着它,不再为躲一次网络请求加配置桩。
    """
    env = {
        key: value
        for key, value in os.environ.items()
        if not any(mark in key.upper() for mark in ("KEY", "TOKEN", "SECRET", "PASSWORD"))
        and not key.startswith("XDG_")
    }
    env["HOME"] = str(home)
    env["XDG_RUNTIME_DIR"] = str(runtime)
    env["XDG_CONFIG_HOME"] = str(home / ".config")
    env["XDG_DATA_HOME"] = str(home / ".local/share")
    env["XDG_CACHE_HOME"] = str(home / ".cache")
    env["XDG_STATE_HOME"] = str(home / ".local/state")
    env.pop("MIYU_HOME", None)
    return env


class Daemon:
    """一个 daemon 进程。`explicit_home` 决定走不走 MIYU_HOME 那条路。"""

    def __init__(self, binary, home_root, runtime_root, explicit_home, port):
        # HOME 决定「没设 MIYU_HOME 时」算出来的默认家目录，必须一起隔离，
        # 否则测试会打到开发机真正的 ~/.miyu 上。
        env = clean_env(home_root, runtime_root)
        if explicit_home:
            env["MIYU_HOME"] = str(home_root / ".miyu")
        self.explicit = explicit_home
        self.log = home_root / f"daemon-{'explicit' if explicit_home else 'default'}.log"
        handle = open(self.log, "wb")
        self.proc = subprocess.Popen(
            [str(binary), "__daemon", "--port", str(port), "--bind", "127.0.0.1"],
            env=env,
            stdout=handle,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )

    def wait_exit(self, timeout):
        try:
            return self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            return None

    def alive(self):
        return self.proc.poll() is None

    def output(self):
        try:
            return self.log.read_text(errors="replace")
        except OSError:
            return ""

    def kill(self):
        if self.alive():
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                self.proc.terminate()
            self.wait_exit(10)


def scenario_same_home(binary, workdir):
    """设了 MIYU_HOME 和没设,指的是同一个家目录 —— 第二个必须让位。"""
    print("\n[1] 同一个家目录,两种 MIYU_HOME 写法")
    home = workdir / "case1"
    (home / ".miyu").mkdir(parents=True)
    runtime = workdir / "run1"
    runtime.mkdir()

    first = Daemon(binary, home, runtime, explicit_home=True, port=free_port())
    # 先让占位的那个把锁抢稳；它要开库、建目录,给够时间。
    time.sleep(6)
    check("先起的 daemon 活着", first.alive(), f"pid={first.proc.pid}")

    second = Daemon(binary, home, runtime, explicit_home=False, port=free_port())
    code = second.wait_exit(30)
    check("后起的 daemon 主动退出", code is not None, f"exit={code}")
    text = second.output()
    check(
        "退出时说清了原因",
        ("让位" in text) or ("standing down" in text),
        text.strip().splitlines()[-1][:110] if text.strip() else "(无输出)",
    )
    check("先起的那个没被影响", first.alive())

    # 让位要赶在占资源之前:第二个 daemon 连自己那个 runtime_dir 都不该建
    # 出来,更别说开库、抢端口、拉起 miyu-voice。
    names = sorted(p.name for p in runtime.iterdir() if p.is_dir())
    check(
        "让位赶在建运行时目录之前",
        len(names) == 1,
        f"runtime 目录: {names}",
    )

    lock = home / ".miyu" / "daemon.lock"
    ok = False
    if lock.exists():
        try:
            record = json.loads(lock.read_text())
            ok = record.get("pid") == first.proc.pid
        except (ValueError, OSError):
            ok = False
    check("锁文件记着在位那个 daemon 的 pid", ok)

    first.kill()
    second.kill()


def scenario_separate_homes(binary, workdir):
    """不同家目录互不干扰 —— 否则一跑测试就把开发机的 daemon 顶掉。"""
    print("\n[2] 两个不同的家目录")
    runtime = workdir / "run2"
    runtime.mkdir()
    homes = []
    for index in (1, 2):
        home = workdir / f"case2-{index}"
        (home / ".miyu").mkdir(parents=True)
        homes.append(Daemon(binary, home, runtime, explicit_home=True, port=free_port()))
    time.sleep(8)
    check("第一个家目录的 daemon 活着", homes[0].alive())
    check("第二个家目录的 daemon 也活着", homes[1].alive())
    for daemon in homes:
        daemon.kill()


def scenario_lock_released(binary, workdir):
    """在位的 daemon 走了,锁要能交给下一个。"""
    print("\n[3] 让位后锁能再被拿到")
    home = workdir / "case3"
    (home / ".miyu").mkdir(parents=True)
    runtime = workdir / "run3"
    runtime.mkdir()

    first = Daemon(binary, home, runtime, explicit_home=True, port=free_port())
    time.sleep(6)
    check("占位的 daemon 起来了", first.alive())
    first.kill()
    time.sleep(2)

    second = Daemon(binary, home, runtime, explicit_home=False, port=free_port())
    time.sleep(6)
    check("前一个走了之后,新 daemon 能起来", second.alive())
    second.kill()


def scenario_cli_is_told_why(binary, workdir):
    """CLI 探不到 daemon、锁又被占着时,要当场说清楚,而不是起一个注定
    让位的进程、然后让用户对着「启动超时」发呆。"""
    print("\n[4] CLI 撞上跑在别处的 daemon")
    home = workdir / "case4"
    (home / ".miyu").mkdir(parents=True)
    runtime = workdir / "run4"
    runtime.mkdir()

    holder = Daemon(binary, home, runtime, explicit_home=True, port=free_port())
    time.sleep(6)
    check("占位的 daemon 起来了", holder.alive())

    env = clean_env(home, runtime)  # 不设 MIYU_HOME —— 就是分叉的来源
    done = subprocess.run(
        [str(binary), "daemon", "start"],
        env=env,
        capture_output=True,
        text=True,
        timeout=90,
    )
    output = (done.stdout or "") + (done.stderr or "")
    check("CLI 没有假装成功", done.returncode != 0, f"exit={done.returncode}")
    check(
        "说清了是同一个家目录、另一个运行时目录",
        ("runtime=" in output) and (str(holder.proc.pid) in output),
        output.strip().splitlines()[-1][:110] if output.strip() else "(无输出)",
    )
    check(
        "给了可照做的出路",
        ("daemon restart" in output) or ("MIYU_HOME" in output),
    )
    check("占位的 daemon 没被顶掉", holder.alive())
    holder.kill()


def scenario_direct_mode_is_excluded(binary, workdir):
    """直连模式(`MIYU_DIRECT=1`)与 daemon 互斥 —— 哪怕两边算出的
    runtime_dir 不是同一个。直连的 `core.lock` 住在 runtime_dir 底下,单靠
    它的话,没设 MIYU_HOME 的 shell 起的直连 REPL 会跟设了环境变量起来的
    daemon 各锁各的,同时开着同一份数据。"""
    print("\n[5] 直连模式撞上 daemon")
    home = workdir / "case5"
    (home / ".miyu").mkdir(parents=True)
    runtime = workdir / "run5"
    runtime.mkdir()

    holder = Daemon(binary, home, runtime, explicit_home=True, port=free_port())
    time.sleep(6)
    check("daemon 占着这个家目录", holder.alive())

    env = clean_env(home, runtime)  # 不设 MIYU_HOME —— 正是两边分叉的那条路
    env["MIYU_DIRECT"] = "1"
    done = subprocess.run(
        [str(binary), "ask", "ping"],
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    output = (done.stdout or "") + (done.stderr or "")
    check("直连没有被放行", done.returncode != 0, f"exit={done.returncode}")
    check(
        "说清了是被另一个 Miyu 核心占着",
        ("另一个 Miyu 核心" in output) or ("another Miyu core" in output),
        output.strip().splitlines()[-1][:110] if output.strip() else "(无输出)",
    )
    check(
        "点出了占位者身份",
        str(holder.proc.pid) in output,
    )
    check("daemon 没被顶掉", holder.alive())
    holder.kill()


def scenario_direct_mode_alone_is_fine(binary, workdir):
    """没有 daemon 时直连照常能起 —— 别把闸修成谁都进不去。"""
    print("\n[6] 没有 daemon 时直连不受影响")
    home = workdir / "case6"
    (home / ".miyu").mkdir(parents=True)
    runtime = workdir / "run6"
    runtime.mkdir()

    env = clean_env(home, runtime)
    env["MIYU_DIRECT"] = "1"
    done = subprocess.run(
        [str(binary), "ask", "ping"],
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    output = (done.stdout or "") + (done.stderr or "")
    # 没配模型多半会因为别的原因失败,但**不该**是被单例锁挡下。
    blocked = ("另一个 Miyu 核心" in output) or ("another Miyu core" in output)
    check("没有被单例锁误挡", not blocked,
          output.strip().splitlines()[-1][:110] if output.strip() else "(无输出)")
    lock = home / ".miyu" / "daemon.lock"
    check("直连退出后没留下占着的锁", not _lock_is_held(lock))


def _lock_is_held(path):
    """锁还被谁持有吗。拿不到=有人占着。"""
    import fcntl
    if not path.exists():
        return False
    try:
        with open(path, "r+") as handle:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(handle, fcntl.LOCK_UN)
                return False
            except BlockingIOError:
                return True
    except OSError:
        return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=None)
    args = parser.parse_args()

    binary = args.binary or os.environ.get("MIYU_BINARY")
    if not binary:
        target = os.environ.get("CARGO_TARGET_DIR", "target")
        binary = str(Path(target) / "debug" / "miyu")
    binary = Path(binary).resolve()
    if not binary.exists():
        print(f"找不到二进制：{binary}")
        return 2
    print(f"二进制：{binary}")

    workdir = sandbox_dir.make("miyu-singleton-", delete_at_exit=False)
    try:
        scenario_same_home(binary, workdir)
        scenario_separate_homes(binary, workdir)
        scenario_lock_released(binary, workdir)
        scenario_cli_is_told_why(binary, workdir)
        scenario_direct_mode_is_excluded(binary, workdir)
        scenario_direct_mode_alone_is_fine(binary, workdir)
    finally:
        shutil.rmtree(workdir, ignore_errors=True)

    print(f"\n通过 {len(PASS)} / {len(PASS) + len(FAIL)}")
    if FAIL:
        print("失败：" + ", ".join(FAIL))
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())

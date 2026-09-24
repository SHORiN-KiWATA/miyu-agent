#!/usr/bin/env python3
"""中转线工具面按会话种类收口的黑盒验收(09-23)。

中转线(claude-code 等)的工具只从 MCP 桥拿,`miyu tool-call --list` 走的就是桥的
目录。开着语音唤醒时,普通会话的目录里不该有 end_voice_chat——修之前每条会话都有。
子代理那一半要真模型开出子代理才走得到,由 miyu-hosts 的单测
the_bridge_scopes_tools_by_session_kind_like_the_turn_does 覆盖。

用法: run.py <miyu 二进制>      退出码 0 = 普通会话目录里没有 end_voice_chat

隔离:/tmp 下的临时家目录 + 独立 XDG_RUNTIME_DIR + 独立端口,不碰线上 8300。
PATH 换成空目录:配置里开了语音唤醒,daemon 会去 PATH 和程序所在目录找 miyu-voice
拉起来(开麦克风);所以二进制也别用装在 /usr/bin 或 ~/.local/bin 的那份——
那两处旁边就放着 miyu-voice。
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402

PORT = "18397"


def leftovers(home: Path) -> list[str]:
    """命令行里带着这个临时家目录的进程(daemon 与它拉起的子进程)。"""
    found = []
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            cmdline = (proc / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
            environ = (proc / "environ").read_bytes()
        except OSError:
            continue
        if str(home) in cmdline or f"MIYU_HOME={home}".encode() in environ:
            found.append(proc.name)
    return found


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    miyu = Path(sys.argv[1]).resolve()
    if (miyu.parent / "miyu-voice").exists():
        print(f"{miyu.parent} 里有 miyu-voice,开着语音唤醒会把它拉起来开麦克风,换一个二进制")
        return 2
    home = sandbox_dir.make("relay-face-", delete_at_exit=False)
    run = home / "run"
    empty_path = home / "empty-path"
    for path in (run, empty_path, home / "config"):
        path.mkdir()
    # 供应商只为过配置校验(active_provider 必填),列目录不会去调它。
    config = {
        "config_version": 3,
        "oobe_done": True,
        "active_provider": "codex",
        "active_provider_models": [{"provider_id": "codex", "model": "gpt-5.6-luna"}],
        "providers": [
            {
                "id": "codex",
                "display_name": "Codex",
                "base_url": "",
                "protocol": "codex",
                "models": ["gpt-5.6-luna"],
                "default_model": "gpt-5.6-luna",
                "enabled": True,
            }
        ],
        "voice": {"enabled": True},
        "memory": {"enabled": False},
    }
    (home / "config" / "config.jsonc").write_text(json.dumps(config), encoding="utf-8")
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "MIYU_HOME")
    }
    env.update(
        MIYU_HOME=str(home),
        XDG_RUNTIME_DIR=str(run),
        PATH=str(empty_path),
        LANG="zh_CN.UTF-8",
    )
    daemon = subprocess.Popen(
        [str(miyu), "daemon", "--port", PORT],
        env=env,
        stdout=(home / "daemon.log").open("w"),
        stderr=subprocess.STDOUT,
    )
    try:
        deadline = time.time() + 30
        while time.time() < deadline and not any(run.rglob("*.sock")):
            time.sleep(0.3)
        if not any(run.rglob("*.sock")):
            print("daemon 没起来,日志:\n" + (home / "daemon.log").read_text()[-2000:])
            return 2
        listed = subprocess.run(
            [str(miyu), "tool-call", "--list"],
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        names = [line.split("\t")[0].strip() for line in listed.stdout.splitlines() if line.strip()]
        if "ask_question" not in names:
            print(f"目录不对劲(连 ask_question 都没有),原样输出:\n{listed.stdout}{listed.stderr}")
            return 2
        leaked = "end_voice_chat" in names
        verdict = "FAIL:普通会话的目录里有 end_voice_chat" if leaked else "PASS:普通会话的目录里没有 end_voice_chat"
        print(f"{verdict}(开着语音唤醒,桥目录共 {len(names)} 件)  {miyu}")
        return 1 if leaked else 0
    finally:
        # `miyu daemon` 只是启动器:真正的 __daemon 脱离出去单独跑,只关启动器会把它
        # 留在后台(家目录都删了它还占着端口)。先按同一个家目录让它自己停,再清剩下的。
        subprocess.run([str(miyu), "daemon", "stop"], env=env, capture_output=True, timeout=30)
        if daemon.poll() is None:
            daemon.terminate()
            try:
                daemon.wait(timeout=15)
            except subprocess.TimeoutExpired:
                daemon.kill()
        for sig in (signal.SIGTERM, signal.SIGKILL):
            rest = leftovers(home)
            if not rest:
                break
            for pid in rest:
                try:
                    os.kill(int(pid), sig)
                except OSError:
                    pass
            time.sleep(1)
        rest = leftovers(home)
        shutil.rmtree(home, ignore_errors=True)
        print(f"残留进程: {len(rest)}" + (f" {rest}" if rest else ""))


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""tool-call 目录同源实测(08-16 dev 实测坑回归)。

坑:dev 会话里 `miyu tool-call --list` 展示普通人格全量目录(客户端按
MIYU_TURN_MODE 环境变量本地建表,run_command 并不注入它),实测逐个调用
全报 unknown tool;报错也无引导。修后 --list/--describe 走 ToolCatalog
IPC,与 ToolCall 同一条会话→模式→registry 解析链。

复验路径:隔离 home + 隔离 XDG_RUNTIME_DIR + 独立端口起 debug daemon →
IPC 建 dev 会话 → 真 CLI 断言:dev/normal 目录差异、unknown+--list 路标、
近似建议、describe 合同、正常调用不受影响。绝不触碰线上 8300 daemon。
注意:BASE 不能太深,unix socket 有 SUN_LEN(108B)路径上限。
"""

import importlib.util
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
MIYU = REPO / "target" / "debug" / "miyu"
BASE = Path(__file__).resolve().parent
HOME = BASE / "home"
RUN = BASE / "xdg-run"
PORT = 18392

spec = importlib.util.spec_from_file_location(
    "persona_ab", REPO / "testkit" / "persona-ab" / "run.py"
)
persona_ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(persona_ab)


def build_home():
    for path in (HOME, RUN):
        if path.exists():
            shutil.rmtree(path)
    (HOME / "config").mkdir(parents=True)
    RUN.mkdir(parents=True)
    cfg = persona_ab.load_real_config()
    for key in ("platforms", "voice", "alarm"):
        cfg.pop(key, None)
    cfg.setdefault("prompt", {})["active_persona"] = ""
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def env(extra=None):
    e = dict(os.environ)
    e["MIYU_HOME"] = str(HOME)
    e["XDG_RUNTIME_DIR"] = str(RUN)
    e.pop("MIYU_DIRECT", None)
    e.pop("MIYU_SESSION", None)
    e.pop("MIYU_TURN_MODE", None)
    e["LANG"] = "zh_CN.UTF-8"
    if extra:
        e.update(extra)
    return e


def find_socket():
    for p in RUN.rglob("*.sock"):
        return p
    return None


def ipc(command: dict):
    """裸 IPC 客户端:u32 大端长度前缀 + JSON,内嵌 tag `command`。"""
    sock_path = find_socket()
    assert sock_path, "ipc socket not found"
    payload = json.dumps({"version": 3, **command}).encode()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(str(sock_path))
    s.sendall(struct.pack(">I", len(payload)) + payload)
    (length,) = struct.unpack(">I", s.recv(4))
    data = b""
    while len(data) < length:
        chunk = s.recv(length - len(data))
        if not chunk:
            break
        data += chunk
    s.close()
    return json.loads(data)


def cli(args, session=None):
    extra = {"MIYU_SESSION": session} if session else None
    proc = subprocess.run(
        [str(MIYU), *args],
        env=env(extra),
        capture_output=True,
        text=True,
        timeout=60,
    )
    return proc.returncode, proc.stdout, proc.stderr


results = []


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(f"{'✅' if ok else '❌'} {name}  {detail}")


def main():
    build_home()
    daemon = subprocess.Popen(
        [str(MIYU), "daemon", "--port", str(PORT)],
        env=env(),
        stdout=(BASE / "daemon.log").open("w"),
        stderr=subprocess.STDOUT,
    )
    try:
        for _ in range(60):
            if find_socket():
                break
            time.sleep(0.5)
        assert find_socket(), "daemon socket never appeared"
        time.sleep(1)

        resp = ipc({"command": "create_session", "name": "toolcall live",
                    "switch": False, "kind": None, "mode": "dev"})
        assert resp.get("type") == "admin_result", resp
        dev = resp["data"]["session"]["session_id"]
        print(f"dev session: {dev}")

        # 1. dev 会话 --list:只列 dev 目录
        code, out, err = cli(["tool-call", "--list"], session=dev)
        names = [line.split("\t")[0] for line in out.strip().splitlines() if line]
        check("dev --list 含 run_command", "run_command" in names, f"{len(names)} tools")
        check("dev --list 不含 trash_path/scientific_calculator",
              "trash_path" not in names and "scientific_calculator" not in names)
        check("dev --list stderr 标注 dev 模式", "dev" in err, err.strip())

        # 2. 无会话 --list:普通目录
        code, out, err = cli(["tool-call", "--list"])
        names_normal = [line.split("\t")[0] for line in out.strip().splitlines() if line]
        check("normal --list 含 trash_path+scientific_calculator",
              "trash_path" in names_normal and "scientific_calculator" in names_normal,
              f"{len(names_normal)} tools")

        # 3. dev 调 normal-only 工具:unknown + --list 路标(实测原始坑)
        code, out, err = cli(
            ["tool-call", "scientific_calculator", '{"expression":"1+1"}'], session=dev)
        check("dev 调 scientific_calculator 报 unknown",
              code != 0 and "unknown tool" in err, err.strip()[:120])
        check("报错带 --list 路标", "--list" in err)

        # 4. 拼错名:近似建议
        code, out, err = cli(["tool-call", "run_comand", "{}"], session=dev)
        check("拼错 run_comand 建议 run_command", "run_command" in err, err.strip()[:120])

        # 5. describe 同源
        code, out, err = cli(["tool-call", "run_command", "--describe"], session=dev)
        ok = False
        try:
            desc = json.loads(out)
            ok = desc["name"] == "run_command" and "command" in desc["parameters"]["properties"]
        except Exception:
            pass
        check("dev --describe run_command 返回合同", code == 0 and ok)

        # 6. 正常调用不受影响
        code, out, err = cli(["tool-call", "job_status", "{}"], session=dev)
        check("dev 调 job_status 正常", code == 0 and '"ok": true' in out, out.strip()[:80])

        # 7. normal 会话同名工具仍可用(对照)
        code, out, err = cli(["tool-call", "scientific_calculator", '{"expression":"1+1"}'])
        check("normal 调 scientific_calculator 正常", code == 0 and "2" in out, out.strip()[:80])
    finally:
        # `miyu daemon` starter 双 fork 分离真 daemon,terminate 只能杀
        # starter,残留进程会占死端口(实测踩坑)——用 CLI stop 走正门。
        subprocess.run([str(MIYU), "daemon", "stop"], env=env(),
                       capture_output=True, timeout=30)
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
        for path in (HOME, RUN):
            if path.exists():
                shutil.rmtree(path)

    failed = [r for r in results if not r[1]]
    print(f"\n=== {len(results) - len(failed)}/{len(results)} 通过 ===")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()

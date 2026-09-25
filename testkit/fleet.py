#!/usr/bin/env python3
"""走查红绿账（09-25）：按 `testkit/fleet.json` 一个一个跑黑盒走查，和已知红账对账。

    python3 testkit/fleet.py --list                       列出登记的走查
    python3 testkit/fleet.py [--bin 路径] [--only a,b]    跑（默认 target/debug/miyu），出报告
    python3 testkit/fleet.py --check [--against-ref HEAD^] 只查账本本身（CI 用）

判定：退出码 0 且输出里没有「N/M passed」式的少过，才算绿。账上该绿的红了，当场复跑一次
（AGENTS §8.5：并行抖动复跑一次，第二次仍红才算问题）。对账结果四种：
    新红    账上记着该绿、两次都红——退出码 1，就是要查的回归
    抖动    账上记着该绿、第一次红、复跑绿——不判失败，报告里单列，反复出现就该去修走查
    红转绿  账上记着红、这次绿了——从 known_red 里划掉（账本只减不增）
    旧红    账上记着红、这次还红——照旧

每项一个全新的 MIYU_HOME（跑完留着，daemon 日志在里面），端口固定（见 fleet.json 的 `env`），同一时刻只跑一项：走查量的是
终端时序，并行会互相抢 CPU 和端口。产物放在 ~/.cache/miyu-fleet/<时间>/，不进 /tmp。

绝不碰线上 8300 daemon：每项都有自己的家目录和端口；--bin 给绝对路径。

账上没收的（09-25 盘点）：
    要真模型 / 网络   tui/live.py、tui/prepare_edit_live.py、compact-quality、memory-quality、persona-ab、
                      dev-smoke、toolcall-bridge、pdf、reasoning-passback、relay-compact、claude-code、codex、
                      antigravity、agy-reuse/live.py、cache-forensics 的 *_live、opencode-zen、model-meta、
                      webui-artifact/run.py（桩里嵌了远程图片和 CDN 脚本）、daemon-singleton（发一次 Zen 请求）
    要真 kitty / GUI  kitty-image、mermaid/terminal.py、tui/kitty_shot.py、tui-demo、repl-cursor 的 trail/copy、
                      notify/kitty_probe、chafa-compat、webui-jitter、herdr/real_herdr.py
    要音频 / 真人     voice、qq-voice、tui/pointer_leave_probe.py、terminal/ambwidth
    要事先起好的 daemon  qq-video、multi-user/{jump,typing}_probe.py、dashboard-scripts
    只量不判          tui 的 page_follow / thinking_lag / prepare_edit / smooth_probe / latency_probe /
                      bg_latency、repl-smoke/run.py、shellhook-question、webui-session-delete、webui_verify
    要 release 构建   tui/budget.py
判定口径：退出码；只打印结论不设退出码的走查在清单里写 `pass`（输出里必须出现的正则）。
"""

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEDGER = ROOT / "testkit" / "fleet.json"
PASSED = re.compile(r"(\d+)\s*/\s*(\d+)\s+passed")
# 走查的进程多半坐在某个 herdr pane 里；漏给被测的 miyu 会把人正在看的侧栏搅乱（09-23）。
SCRUBBED = ("MIYU_SESSION", "MIYU_DIRECT", "MIYU_TURN_MODE", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
            "XDG_STATE_HOME", "XDG_CACHE_HOME")


def load(path=LEDGER):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def check(ledger, previous=None):
    """账本本身的毛病。`previous` 是旧版账本：红账只减不增。"""
    problems = []
    names = [entry.get("name") for entry in ledger.get("walkthroughs", [])]
    if len(names) != len(set(names)):
        problems.append("走查名字重复")
    for entry in ledger.get("walkthroughs", []):
        name = entry.get("name") or "?"
        argv = entry.get("argv") or []
        if not argv:
            problems.append(f"{name}: 没有 argv")
            continue
        scripts = [arg for arg in argv if arg.endswith(".py") or arg.endswith(".sh")]
        for script in scripts:
            if not (ROOT / script).exists():
                problems.append(f"{name}: 找不到 {script}")
        if not isinstance(entry.get("timeout", 600), int):
            problems.append(f"{name}: timeout 要是整数秒")
        if entry.get("pass"):
            try:
                re.compile(entry["pass"])
            except re.error as error:
                problems.append(f"{name}: pass 不是合法的正则（{error}）")
    known = {item.get("name"): item for item in ledger.get("known_red", [])}
    for name, item in known.items():
        if name not in names:
            problems.append(f"known_red 里的 {name} 不在走查清单里")
        if not (item.get("reason") or "").strip():
            problems.append(f"known_red 里的 {name} 没写原因")
    if previous is not None:
        before = {item.get("name") for item in previous.get("known_red", [])}
        for name in sorted(set(known) - before):
            problems.append(f"红账只减不增：新记了 {name}（修好它，或者在提交里说清为什么非记不可）")
    return problems


def ledger_at(ref):
    """某个提交里的账本。那时还没有账本（建账的那个提交）就返回 None：没得比。"""
    run = subprocess.run(["git", "show", f"{ref}:testkit/fleet.json"], cwd=ROOT,
                         capture_output=True, text=True)
    return json.loads(run.stdout) if run.returncode == 0 else None


def verdict(code, log_text, entry):
    """退出码 0、没有少过的「N/M passed」、清单里写了 `pass` 的还得在输出里找得到它，才算绿。
    有的走查只打印结论不设退出码，靠 `pass` 兜住。"""
    if code != 0:
        return False
    for done, total in PASSED.findall(log_text):
        if int(done) < int(total):
            return False
    wanted = entry.get("pass")
    return not wanted or re.search(wanted, log_text) is not None


def summary_line(log_text):
    """报告里给人看的那一行：最后一条像结论的输出。"""
    lines = [line.strip() for line in log_text.splitlines() if line.strip()]
    for line in reversed(lines):
        if PASSED.search(line) or re.search(r"通过|PASS|FAIL|红|✗", line):
            return line[:160]
    return lines[-1][:160] if lines else ""


def run_one(entry, binary, out, base_env, tag=""):
    # 每项一个家目录、跑完留着：daemon 的详细日志在家目录里，红了要靠它查（共用一个的话
    # 下一项一开跑就把上一项的日志抹了）。复跑的那次另起一个（`tag`），两次的都留着。
    home = out / "homes" / (entry["name"] + tag)
    shutil.rmtree(home, ignore_errors=True)
    for sub in ("rt", "strt", "artifacts"):
        (out / sub).mkdir(parents=True, exist_ok=True)
    env = dict(base_env)
    env.update(BIN=str(binary), MIYU_BIN=str(binary), MIYU_HOME=str(home),
               MIYU_TUI_RUNTIME=str(out / "rt"), MIYU_ST_RUNTIME=str(out / "strt"),
               OUT=str(out / "artifacts"))
    for key, value in entry.get("env", {}).items():
        env[key] = str(value).replace("{bin}", str(binary))
    argv = [arg.replace("{bin}", str(binary)) for arg in entry["argv"]]
    if argv[0] == "python3":
        argv[0] = sys.executable
    log_path = out / "logs" / f"{entry['name']}{tag}.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    with log_path.open("w", encoding="utf-8") as log:
        # 单独成组：超时时连同它起的 daemon、桩、TUI 一起收掉。
        process = subprocess.Popen(argv, cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=entry.get("timeout", 600))
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            code = "timeout"
    elapsed = time.monotonic() - started
    text = log_path.read_text(encoding="utf-8", errors="replace")
    green = code != "timeout" and verdict(code, text, entry)
    return {"name": entry["name"], "green": green, "exit": code, "seconds": round(elapsed, 1),
            "summary": summary_line(text), "log": str(log_path)}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument("--bin", default=str(ROOT / "target" / "debug" / "miyu"))
    parser.add_argument("--only", help="逗号分隔的走查名")
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--check", action="store_true", help="只查账本")
    parser.add_argument("--against-ref", help="--check 时和这个提交里的账本比：红账只减不增")
    parser.add_argument("--out", help="产物目录（默认 ~/.cache/miyu-fleet/<时间>）")
    args = parser.parse_args()

    ledger = load()
    if args.check:
        previous = ledger_at(args.against_ref) if args.against_ref else None
        problems = check(ledger, previous)
        for problem in problems:
            print(f"✗ {problem}")
        count = len(ledger.get("walkthroughs", []))
        reds = len(ledger.get("known_red", []))
        print(f"账本 {count} 项，已知红 {reds} 项" if not problems else "账本有问题")
        return 1 if problems else 0

    entries = ledger["walkthroughs"]
    if args.only:
        wanted = set(args.only.split(","))
        entries = [entry for entry in entries if entry["name"] in wanted]
    if args.list:
        for entry in entries:
            print(f"{entry['name']:<28} {' '.join(entry['argv'])}")
        return 0

    binary = Path(args.bin).resolve()
    if not binary.exists():
        print(f"找不到 miyu 二进制：{binary}（先 cargo build）")
        return 2
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = Path(args.out) if args.out else Path.home() / ".cache" / "miyu-fleet" / stamp
    out.mkdir(parents=True, exist_ok=True)
    base_env = {key: value for key, value in os.environ.items()
                if not key.startswith("HERDR_") and key not in SCRUBBED}
    # 账本里的端口是默认值：外面显式给了就用外面的（两遍同时跑时各用一组端口）。
    for key, value in ledger.get("env", {}).items():
        base_env.setdefault(key, str(value))
    known = {item["name"]: item for item in ledger.get("known_red", [])}

    results = []
    for index, entry in enumerate(entries, 1):
        print(f"[{index}/{len(entries)}] {entry['name']} …", flush=True)
        result = run_one(entry, binary, out, base_env)
        expected_red = entry["name"] in known
        if expected_red:
            result["ledger"] = "红转绿" if result["green"] else "旧红"
        elif result["green"]:
            result["ledger"] = "绿"
        else:
            print(f"    红了，复跑一次（{result['summary']}）", flush=True)
            retry = run_one(entry, binary, out, base_env, tag="-retry")
            result["retry"] = retry
            result["ledger"] = "抖动" if retry["green"] else "新红"
        results.append(result)
        print(f"    {result['ledger']}  exit={result['exit']}  {result['seconds']}s  {result['summary']}",
              flush=True)

    report = {"binary": str(binary), "results": results}
    (out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    fresh = [r["name"] for r in results if r["ledger"] == "新红"]
    healed = [r["name"] for r in results if r["ledger"] == "红转绿"]
    flaky = [r["name"] for r in results if r["ledger"] == "抖动"]
    print(f"\n{len(results)} 项：绿 {sum(r['ledger'] == '绿' for r in results)}，"
          f"新红 {len(fresh)}，抖动 {len(flaky)}，旧红 {sum(r['ledger'] == '旧红' for r in results)}，"
          f"红转绿 {len(healed)}")
    if fresh:
        print("新红（回归）：" + "、".join(fresh))
    if flaky:
        print("抖动（第一次红、复跑绿）：" + "、".join(flaky))
    if healed:
        print("红转绿，从 known_red 里划掉：" + "、".join(healed))
    print(f"报告：{out / 'report.json'}")
    return 1 if fresh else 0


if __name__ == "__main__":
    sys.exit(main())

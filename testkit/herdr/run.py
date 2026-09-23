#!/usr/bin/env python3
"""herdr 状态上报：在 pane 里报什么、不在 pane 里报不报。

用户 09-20：让 Miyu 出现在 herdr 的 agents 侧栏，带状态色。走的是 herdr 给外部
agent 留的官方接口 `herdr pane report-agent`。

这个走查**不需要真的 herdr**：把 `HERDR_BIN_PATH` 指向一个只把 argv 记进文件的
假 herdr，然后跑一轮真对话，看上报序列对不对。真 herdr 那边的效果（侧栏长什么样）
只能人眼看，但「我们这侧报得对不对」这里全验得了。

量这些：

1. 不在 herdr 里（环境变量不全）时**一次都不报**——绝大多数用户不跑 herdr，
   这段代码对他们必须是零存在感；
2. 在 pane 里时：REPL 起来报 idle、回合开始报 working、说完报 idle；
3. `--seq` 严格单调递增（herdr 会丢掉同一 source 的过期序号）；
4. `--source` 全程是同一个（一个 pane 最多认 32 个不同 source，不能每轮换）；
5. 退出报 release-agent；
6. 假 herdr **返回非零**时回合照样跑完——上报失败必须静默。

跑法（先 cargo build）：

    python3 testkit/herdr/run.py
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("MIYU_BIN", ROOT / "target" / "debug" / "miyu"))
SMOKE = ROOT / "testkit" / "repl-smoke"
HOME = Path(os.environ.get("MIYU_HOME", "/tmp/miyu-herdr/home"))
RUNTIME = os.environ.get("RUNTIME", "/tmp/mx-herdr")
PORT = int(os.environ.get("PORT", "18471"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18479"))
OUT = Path(os.environ.get("OUT", Path.home() / ".cache" / "miyu-herdr"))
COLS, ROWS = 100, 40
PANE_ID = "w9:pTEST"
LOG = OUT / "herdr-calls.jsonl"


def write_fake_herdr(exit_code=0):
    """只把 argv 记下来的假 herdr。

    真 herdr 在用户机器上跑着，走查绝不能去碰它——写操作会顶掉 claude 的状态
    权威。假的既安全又能把参数验死。
    """
    path = OUT / "fake-herdr"
    # **也照着真 herdr 丢过期序号**：它按 source 记水位，序号不大于水位的上报
    # 直接丢。第一版的假 herdr 来者不拒，于是「新进程从 1 重新数」这个 bug
    # 一路漏到用户那儿（09-20：重开一次侧栏就彻底不显示了）。假的要在**这一点上
    # 像真的**，否则测了也白测。
    path.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, sys, time\n"
        f"log = {str(LOG)!r}\n"
        f"marks = {str(LOG) + '.seq'!r}\n"
        "argv = sys.argv[1:]\n"
        "def flag(name):\n"
        "    return argv[argv.index(name) + 1] if name in argv else None\n"
        "source, seq = flag('--source'), flag('--seq')\n"
        "stale = False\n"
        "unsequenced = source is not None and seq is None\n"
        "if source and seq is not None:\n"
        "    high = {}\n"
        "    if os.path.exists(marks):\n"
        "        high = json.load(open(marks, encoding='utf-8'))\n"
        "    if int(seq) <= high.get(source, -1):\n"
        "        stale = True\n"
        "    else:\n"
        "        high[source] = int(seq)\n"
        "        json.dump(high, open(marks, 'w', encoding='utf-8'))\n"
        "with open(log, 'a', encoding='utf-8') as handle:\n"
        "    handle.write(json.dumps({'at': time.time(), 'argv': argv,\n"
        "                             'stale': stale, 'unsequenced': unsequenced},\n"
        "                            ensure_ascii=False) + '\\n')\n"
        f"sys.exit({exit_code})\n",
        encoding="utf-8",
    )
    path.chmod(0o755)
    return path


def calls(include_stale=False):
    """假 herdr 收到的调用。默认**只算没被当成过期丢掉的**——被丢掉的那些在
    真 herdr 那边等于没发生，侧栏上什么都不会变。"""
    if not LOG.exists():
        return []
    rows = [json.loads(line) for line in LOG.read_text(encoding="utf-8").splitlines()]
    return rows if include_stale else [row for row in rows if not row.get("stale")]


def wait_http(url, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except urllib.error.HTTPError:
            return True
        except Exception:
            time.sleep(0.3)
    return False


def arg_of(argv, flag):
    return argv[argv.index(flag) + 1] if flag in argv else None


def run_repl(tui, herdr_env, prompt, seconds=25, answer_after=None, capture=None):
    """起一个 REPL，发一句，等它说完，然后退出。

    `answer_after` = 过这么多秒之后按一下回车。她反问时提问面板是盖上去的，
    回车 = 选中第一个选项并回答，这一轮接着往下跑。
    """
    import pty
    import fcntl
    import struct
    import termios
    import select

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    process = subprocess.Popen(
        [str(BIN)], stdin=slave, stdout=slave, stderr=slave,
        env=dict(tui.ENV, **herdr_env), cwd=str(HOME), close_fds=True,
        preexec_fn=lambda: (os.setsid(), fcntl.ioctl(1, termios.TIOCSCTTY, 0)),
    )
    os.close(slave)
    alive = [True]

    sink = bytearray()

    def pump():
        while alive[0]:
            ready, _, _ = select.select([master], [], [], 0.1)
            if not ready:
                continue
            try:
                chunk = os.read(master, 65536)
                if not chunk:
                    return
                if capture is not None:
                    sink.extend(chunk)
            except OSError:
                return

    threading.Thread(target=pump, daemon=True).start()
    time.sleep(3.0)
    os.write(master, f"{prompt}\r".encode())
    if answer_after is not None:
        time.sleep(answer_after)
        os.write(master, b"\r")
        time.sleep(max(seconds - answer_after, 0))
    else:
        time.sleep(seconds)
    # 空输入时 **Ctrl+D** 是退出（`LiveEditorAction::Exit`，走正常收尾路径）。
    # Ctrl+C 不行：它是「中断」，而且两下也没让这个 REPL 退出。
    os.write(master, b"\x04")
    time.sleep(3.0)
    exited = process.poll()
    if exited is None:
        print("    [探针] Ctrl+D 之后还没退出", flush=True)
    alive[0] = False
    if capture is not None:
        (OUT / capture).write_bytes(bytes(sink))
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()


def require_free_port(port, who):
    """这个端口上不能已经有人在听，否则这一跑测的是别人的进程。"""
    with socket.socket() as probe:
        if probe.connect_ex(("127.0.0.1", port)) == 0:
            raise RuntimeError(
                f"{who}的端口 {port} 上已经有别的进程在听。"
                f"先清掉它（`ss -ltnp | grep {port}` 找 pid），或者给这一跑"
                f"换一套端口（PORT= / STUB_PORT=）。"
            )


def restart_stub_with_ask(stub):
    """把桩换成**会提问**的那一只，并当场验证它真的会提问。

    第一版只 `terminate()` 就接着起新的：旧桩没死透、端口还占着，新桩起不来，
    `wait_http` 探到的还是旧桩——于是那一场根本没提问，走查却把「没报 blocked」
    当成产品的错（09-20 白查一轮）。杀干净、等端口真的空、起新的、再发一条
    请求确认回的是 `ask_question`：无声失败变成当场报错。
    """
    stub.kill()
    stub.wait(timeout=10)
    deadline = time.time() + 15
    while time.time() < deadline:
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{STUB_PORT}/v1/models", timeout=1)
        except Exception:
            break
        time.sleep(0.3)
    else:
        raise RuntimeError("旧桩没退干净，端口还在应答")
    fresh = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_REPLY="好了", STUB_ASK="1"),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models")
    probe = urllib.request.Request(
        f"http://127.0.0.1:{STUB_PORT}/v1/chat/completions",
        data=json.dumps(
            {"model": "stub", "stream": True, "messages": [{"role": "user", "content": "hi"}]}
        ).encode(),
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(probe, timeout=10) as response:
        if b"ask_question" not in response.read():
            raise RuntimeError("换上来的桩不会提问，这一场测不出东西")
    return fresh


def main():
    if not BIN.exists():
        print(f"! 先 cargo build：{BIN} 不存在", file=sys.stderr)
        return 2
    if HOME.exists():
        shutil.rmtree(HOME)
    Path(RUNTIME).mkdir(exist_ok=True)
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    sys.path.insert(0, str(ROOT / "testkit" / "tui"))
    import run as tui

    tui.HOME, tui.PORT, tui.STUB_PORT, tui.BIN = HOME, PORT, STUB_PORT, BIN
    tui.COLS, tui.ROWS = COLS, ROWS
    tui.ENV = dict(os.environ, MIYU_HOME=str(HOME), XDG_RUNTIME_DIR=RUNTIME, MIYU_TUI="1")
    for stale in ("HERDR_ENV", "HERDR_PANE_ID", "HERDR_BIN_PATH"):
        tui.ENV.pop(stale, None)
    tui.write_config()

    # 端口上先来后到：已经有人在应答就**当场报错**，别默默借用别人的桩。
    #
    # 09-20 实测：一只 36000 秒前遗留的老桩一直占着这个端口，走查自己起的桩
    # 根本没绑上，所有请求都打给了它——它的环境变量是上一轮的，于是「换成会
    # 提问的桩」那一场怎么都提不了问，红的却记在产品头上。
    require_free_port(STUB_PORT, "桩模型")
    require_free_port(PORT, "沙箱 daemon")
    stub = subprocess.Popen(
        [sys.executable, str(SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_REPLY="好了"),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = None
    report = {}
    try:
        wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models")
        daemon = subprocess.Popen(
            [str(BIN), "__daemon", "--port", str(PORT)],
            env=tui.ENV, cwd=str(HOME),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        wait_http(f"http://127.0.0.1:{PORT}/api/config")
        fake = write_fake_herdr()

        # ── 一、不在 herdr 里：一次都不该报 ──
        run_repl(tui, {}, "第一句", seconds=15)
        report["不在 herdr 里一次都不报"] = not calls()

        # ── 二、在 pane 里：完整序列 ──
        LOG.unlink(missing_ok=True)
        run_repl(
            tui,
            {
                "HERDR_ENV": "1",
                "HERDR_PANE_ID": PANE_ID,
                "HERDR_BIN_PATH": str(fake),
            },
            "第二句",
            seconds=20,
        )
        seen = calls()
        (OUT / "calls.json").write_text(
            json.dumps(seen, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        reports = [c["argv"] for c in seen if c["argv"][:2] == ["pane", "report-agent"]]
        releases = [c["argv"] for c in seen if c["argv"][:2] == ["pane", "release-agent"]]
        states = [arg_of(argv, "--state") for argv in reports]
        report["_上报序列"] = states
        report["在 pane 里报了状态"] = bool(reports)
        report["起来先报 idle"] = states[:1] == ["idle"]
        report["回合中报过 working"] = "working" in states
        report["说完回到 idle"] = states and states[-1] == "idle"
        report["退出时释放了 pane"] = bool(releases)
        # 释放也必须带序号：herdr 按 source 记水位、丢掉排不进序的上报，一条
        # 不带序号的释放会被忽略，侧栏那行就一直留着（用户 09-20 实测）。
        every_call = [c["argv"] for c in calls(True)]
        report["_没带序号的调用"] = [
            argv[1] for argv in every_call if "--seq" not in argv
        ]
        report["每一条上报都带序号（含释放）"] = all(
            "--seq" in argv for argv in every_call
        )
        seqs = [int(arg_of(argv, "--seq") or 0) for argv in reports]
        release_seqs = [int(arg_of(argv, "--seq") or 0) for argv in releases]
        report["释放的序号比之前的都大"] = (
            bool(release_seqs) and bool(seqs) and min(release_seqs) > max(seqs)
        )
        report["_序号"] = seqs
        report["序号严格递增"] = all(a < b for a, b in zip(seqs, seqs[1:]))
        sources = {arg_of(argv, "--source") for argv in reports + releases}
        report["source 全程唯一"] = sources == {"custom:miyu"}
        agents = {arg_of(argv, "--agent") for argv in reports + releases}
        report["agent 标签是 miyu"] = agents == {"miyu"}
        report["pane id 带对了"] = all(argv[2] == PANE_ID for argv in reports + releases)
        report["带上了会话 id"] = any(
            arg_of(argv, "--agent-session-id") for argv in reports
        )

        # ── 三、假 herdr 返回非零：回合照样跑完 ──
        #
        # 判据用 **shellhook 的 stdout**，不数库里的轮：库的位置随布局变
        # （建没建管理员账号）、轮的状态又会被别的场景（供应商冷却）带偏，
        # 数出来的东西反复骗人（09-20 连红三次，全是判据的错不是产品的错）。
        # 回复有没有打出来，是这件事最直接的证据。
        LOG.unlink(missing_ok=True)
        broken = write_fake_herdr(exit_code=3)
        shell = subprocess.run(
            [str(BIN), "--shell-intercept", "--shell", "fish", "--stdin"],
            input="上报会失败的这一句\n",
            env=dict(
                tui.ENV,
                MIYU_TUI="0",
                HERDR_ENV="1",
                HERDR_PANE_ID=PANE_ID,
                HERDR_BIN_PATH=str(broken),
            ),
            cwd=str(HOME), capture_output=True, text=True, timeout=180,
        )
        (OUT / "broken-herdr.txt").write_text(shell.stdout, encoding="utf-8")
        report["_上报失败时的回复"] = shell.stdout.strip()[-40:]
        report["上报失败时照样在报（没有卡住）"] = len(calls(True)) >= 2
        report["上报失败不影响回合完成"] = "好了" in shell.stdout

        # ── 四、**重开一次之后照样报得出来** ──
        #
        # herdr 按 source 记序号水位、丢掉过期序号，而 source 是固定的
        # `custom:miyu`。序号只在进程内单调的话，第二次开 Miyu 报的全部小于
        # 水位，被静默丢光——用户 09-20 实测到的「彻底不显示」。
        LOG.unlink(missing_ok=True)
        pane_env = {
            "HERDR_ENV": "1",
            "HERDR_PANE_ID": PANE_ID,
            "HERDR_BIN_PATH": str(fake),
        }
        run_repl(tui, pane_env, "重开前", seconds=18)
        first_round = [int(arg_of(c["argv"], "--seq") or 0) for c in calls(True)
                       if c["argv"][:2] == ["pane", "report-agent"]]
        run_repl(tui, pane_env, "重开后", seconds=18)
        every = [c for c in calls(True) if c["argv"][:2] == ["pane", "report-agent"]]
        second_round = [int(arg_of(c["argv"], "--seq") or 0) for c in every][len(first_round):]
        report["_两次的序号"] = [first_round, second_round]
        report["重开之后序号不回头"] = (
            bool(first_round) and bool(second_round)
            and min(second_round) > max(first_round)
        )
        report["重开之后的上报没被当成过期丢掉"] = not any(
            c.get("stale") for c in every
        )

        # ── 四点五、她反问时报 blocked，答完报回 working ──
        #
        # 09-20 真机漏的就是后半截：`TurnGuard::resumed()` 写了一次都没调，
        # 答完之后 herdr 侧栏一直红着「在等你回话」，其实她早就接着干活了
        # （编译器的 `never used` 警告先看出来的，走查当时没覆盖这一段）。
        # 桩模型换成会提问的那一只：`STUB_ASK` 是进程启动时读的，得重起。
        LOG.unlink(missing_ok=True)
        stub = restart_stub_with_ask(stub)
        run_repl(
            tui,
            {
                "HERDR_ENV": "1",
                "HERDR_PANE_ID": PANE_ID,
                "HERDR_BIN_PATH": str(fake),
            },
            "问我一个问题",
            seconds=40,
            answer_after=20,
            capture="ask-round.bin",
        )
        (OUT / "ask-round-calls.json").write_text(
            json.dumps([c["argv"] for c in calls(True)], ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
        ask_states = [
            arg_of(c["argv"], "--state")
            for c in calls()
            if c["argv"][:2] == ["pane", "report-agent"]
        ]
        report["_反问那轮的序列"] = ask_states
        report["她反问时报 blocked"] = "blocked" in ask_states
        report["答完报回 working（不是一直红着）"] = (
            "blocked" in ask_states
            and "working" in ask_states[ask_states.index("blocked") + 1 :]
        )
        report["反问那轮最后也回到 idle"] = bool(ask_states) and ask_states[-1] == "idle"

        # ── 五、一次性 / shellhook：收尾要把 pane 还回去，且不许改终端标题 ──
        #
        # 一次性进程跑完就没了。收尾只报 idle 的话，那个 pane 上会永远挂着一个
        # 已经不存在的 miyu——人在终端里说了一句自然语言，侧栏就多个赖着不走的
        # agent。标题同理：改完就退出，没人改回来，标签页被永久改名。
        LOG.unlink(missing_ok=True)
        shell = subprocess.run(
            [str(BIN), "--shell-intercept", "--shell", "fish", "--stdin"],
            input="一次性这一句\n",
            env=dict(
                tui.ENV,
                MIYU_TUI="0",
                HERDR_ENV="1",
                HERDR_PANE_ID=PANE_ID,
                HERDR_BIN_PATH=str(fake),
            ),
            cwd=str(HOME), capture_output=True, text=True, timeout=180,
        )
        (OUT / "shellhook.txt").write_text(shell.stdout, encoding="utf-8")
        # 假 herdr 是一堆各自独立的短进程，日志按**写完时刻**排序，不是按
        # 调用顺序——实测见过 seq 618 的 report-metadata 排在 seq 619 的
        # idle 后面。所以「谁是最后一条」只能按 **序号** 判，不能按行序；
        # 这也正是 herdr 自己认的东西（它按 source 记序号水位）。
        time.sleep(1.5)
        one_shot = calls()
        kinds = [c["argv"][1] for c in one_shot]
        report["_一次性的调用"] = kinds
        report["一次性也报了 working"] = any(
            arg_of(c["argv"], "--state") == "working" for c in one_shot
        )
        all_seq = [int(arg_of(c["argv"], "--seq") or 0) for c in one_shot]
        release_seq = [
            int(arg_of(c["argv"], "--seq") or 0)
            for c in one_shot
            if c["argv"][1] == "release-agent"
        ]
        report["_一次性的序号"] = all_seq
        report["一次性收尾是 release 不是 idle"] = (
            bool(release_seq) and bool(all_seq) and max(release_seq) == max(all_seq)
        )
        report["一次性不改终端标题"] = "\x1b]2;" not in shell.stdout

        # ── 末场、回合**报错**时也要回到 idle ──
        #
        # 放在**最后**：让桩回 HTTP 500 会把供应商拉进冷却（09-19 做的），
        # 后面几场的回合都得等它，18 秒跑不完就成了 interrupted——不是产品
        # 问题，是走查顺序有毒（09-20 踩过一次）。
        #
        # 用户 09-20 实测：中途报错的话 herdr 上一直显示「运行中」。原因是只在
        # 正常收尾那条路上报了 idle，而这个函数有九条出口（run.failed、
        # run.cancelled、断线、Ctrl+C、三处 bail!……）。现在用 Drop 收口。
        LOG.unlink(missing_ok=True)
        stub.terminate()
        stub.wait(timeout=5)
        broken_stub = subprocess.Popen(
            [sys.executable, str(SMOKE / "stub_llm.py")],
            env=dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_HTTP_STATUS="500"),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models")
        run_repl(
            tui,
            {
                "HERDR_ENV": "1",
                "HERDR_PANE_ID": PANE_ID,
                "HERDR_BIN_PATH": str(fake),
            },
            "这一句会失败",
            seconds=25,
        )
        failed = [
            c["argv"] for c in calls() if c["argv"][:2] == ["pane", "report-agent"]
        ]
        failed_states = [arg_of(argv, "--state") for argv in failed]
        (OUT / "failed-calls.json").write_text(
            json.dumps(failed_states, ensure_ascii=False), encoding="utf-8"
        )
        report["_报错那轮的序列"] = failed_states
        report["报错后报过 working"] = "working" in failed_states
        report["报错后回到 idle（不卡在运行中）"] = (
            bool(failed_states) and failed_states[-1] == "idle"
        )
        broken_stub.terminate()
        stub = subprocess.Popen(
            [sys.executable, str(SMOKE / "stub_llm.py")],
            env=dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_REPLY="好了"),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        wait_http(f"http://127.0.0.1:{STUB_PORT}/v1/models")
    finally:
        for process in (daemon, stub):
            if process:
                process.terminate()

    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    passed = sum(1 for value in checks.values() if value)
    for name, value in checks.items():
        print(f"{'✅' if value else '❌'} {name}")
    print(f"\n{passed}/{len(checks)} passed")
    for name, value in report.items():
        if name.startswith("_"):
            print(f"   {name[1:]}: {value}")
    print(f"产物：{OUT}")
    return 0 if passed == len(checks) else 1


if __name__ == "__main__":
    raise SystemExit(main())

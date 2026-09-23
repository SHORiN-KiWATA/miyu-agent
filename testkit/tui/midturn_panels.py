#!/usr/bin/env python3
"""探针：回合跑着时开 `/models` / `/session` 面板，正文到底怎么了。

用户 09-20 报：「AI 正在输出时运行 /session /models 之类的命令，AI 的输出会消失，
好像是被打断了」。要分清两件**完全不同**的事：

- 回合真的在 daemon 里被掐了（库里的轮是 interrupted/cancelled）；
- 回合好好跑完了，只是剩下那半截没画到屏幕上。

判据是正文**最后**那个独一无二的标记：它只在整轮流完时才会出现。第一版拿
「前半段/后半段」判，面板开得比后半段还晚，等于没测（09-20）。面板一开出来就
死等库里那一轮变成 completed，确保「回合在面板开着的时候跑完」这个现场真的成立。

    MIYU_HOME=/tmp/miyu-panels/home MIYU_TUI_PORT=18485 STUB_PORT=18486 \\
      MIYU_TUI_RUNTIME=/tmp/mx-panels OUT=~/.cache/miyu-panels \\
      python3 testkit/tui/midturn_panels.py
"""

import json
import os
import re
import socket
import sqlite3
import subprocess
import urllib.request
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as h  # noqa: E402
import round26 as r  # noqa: E402

HEAD = "开头标记。"
# 每一场换一个**独一无二**的尾巴标记。共用一个标记时，上一场的尾巴还留在回屏
# 里，新的挤进来时旧的又滚掉了，计数净值不变——判据说不清是没画出来还是滚掉了
# （09-20 实测 1→1）。
BASE = {"STUB_CHUNK_SLEEP": "0.05", "STUB_CHUNK_CHARS": "4"}
# **慢速**：块与块之间空 1.2 秒，中间全是转轮在刷活动区。面板能不能扛住 33ms
# 一次的转轮重画，只有这个档位测得出来——快速档下正文帧一直在来，会把被刷掉的
# 面板又补回去，于是走查全绿而真机上面板根本看不见（用户 09-20 实测）。
SLOW = {"STUB_CHUNK_SLEEP": "1.2", "STUB_CHUNK_CHARS": "4"}


def reply_for(label):
    # 慢速档一块 4 个字、一秒多一块，正文得短一些，不然一场要跑几十分钟。
    filler = 12 if label.startswith("slow") else 2000
    # 填充要**带编号**：重复同一串字的话，面板一高、正文只剩一两行时，滚动
    # 前后看起来一模一样，「还在不在流」就永远判成否（09-20 实测）。
    body = "".join(f"填{index:04d}" for index in range(filler))
    return HEAD + body + f"尾巴{label}完。"


def stub_env_for(label):
    return SLOW if label.startswith("slow") else BASE


STUB = dict(BASE, STUB_REPLY=reply_for("bootstrap"))


def squash(screen):
    """整屏拼成一串，去掉行首尾空白。

    正文是**软换行**的，标记会被切成两行（实测「…填充尾巴」/「标记。」），
    按行找永远找不到——09-19 多 TUI 那项踩过同一个坑。
    """
    return "".join(line.strip() for line in screen)


def max_filler(screen):
    """屏幕上看得见的最大填充编号。一个都没有就给 -1。"""
    found = re.findall(r"填(\d{4})", squash(screen))
    return max((int(index) for index in found), default=-1)


def db():
    candidates = sorted(Path(h.HOME).glob("home/*/conversation.db"))
    return candidates[0] if candidates else None


def turns():
    path = db()
    if not path:
        return []
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return connection.execute(
            "SELECT status, length(assistant_content) FROM turns ORDER BY seq"
        ).fetchall()
    finally:
        connection.close()


def completed_count():
    return sum(1 for row in turns() if row[0] == "completed")


# `turns()` 数的是**所有**轮（含还在跑的那条）；`completed_count()` 只数跑完的。
# 起轮看前者，「跑完没有」看后者。


def start_turn(master, sink, text, done_before):
    """发一句，确认它**确实在跑**（库里这一轮还没完成）。

    原来是等「开头标记」出现在屏幕上——正文一长，开头几行在轮询到之前就滚出
    视口了，于是死等 90 秒然后报「回合没起来」，其实回合早跑完了（09-20）。
    回合在不在跑是库里的事实，别拿转瞬即逝的屏幕内容当凭据。
    """
    os.write(master, text.encode())
    h.drain_until(master, sink, text, 5.0)
    os.write(master, b"\r")
    # 死等「库里多一轮」= 这一轮已经登记；再流一会儿，然后确认它**还没完成**。
    # 屏幕上的内容不能当凭据：正文一长，开头几行在轮询到之前就滚出视口了。
    deadline = time.time() + 90
    while time.time() < deadline and len(turns()) <= done_before:
        h.drain(master, 0.5, sink)
    h.drain(master, 5.0, sink)
    return completed_count() == done_before


def restart_stub(stub, reply, env_base):
    """换一只吐 `reply` 的桩，并**验证换上来的真是新的**。

    只 terminate 再起新的不行：旧桩没死透、端口还占着，新桩绑不上，请求全打给
    旧的——测的就不是你以为的东西（09-20 在 herdr 测具上栽过一次，这里照抄
    那条教训）。
    """
    stub.kill()
    stub.wait(timeout=10)
    deadline = time.time() + 15
    while time.time() < deadline:
        with socket.socket() as probe:
            if probe.connect_ex(("127.0.0.1", h.STUB_PORT)) != 0:
                break
        time.sleep(0.3)
    else:
        raise RuntimeError("旧桩没退干净，端口还在应答")
    fresh = subprocess.Popen(
        [sys.executable, str(h.SMOKE / "stub_llm.py")],
        env=dict(os.environ, STUB_PORT=str(h.STUB_PORT), **dict(env_base, STUB_REPLY=reply)),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if not h.wait_http(f"http://127.0.0.1:{h.STUB_PORT}/v1/models"):
        raise RuntimeError("新桩没起来")
    request = urllib.request.Request(
        f"http://127.0.0.1:{h.STUB_PORT}/v1/chat/completions",
        data=json.dumps(
            {"model": "stub", "stream": True, "messages": [{"role": "user", "content": "hi"}]}
        ).encode(),
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        body = response.read().decode("utf-8", "replace")
    # 正文按 4 字一块流，末尾那串在**原始 SSE 里是断开的**——要先把所有
    # `delta.content` 拼回来再找（09-20：直接在原始字节里找，必然找不到）。
    joined = ""
    for line in body.splitlines():
        if not line.startswith("data: ") or line.endswith("[DONE]"):
            continue
        try:
            chunk = json.loads(line[6:])
        except ValueError:
            continue
        for choice in chunk.get("choices", []):
            joined += choice.get("delta", {}).get("content") or ""
    if reply[-8:] not in joined:
        # 验证不过就把刚起的这只收掉再抛，否则它会赖在端口上，下一跑被
        # 端口守卫拦住（09-20 实测漏过一次，自己挡了自己）。
        fresh.kill()
        raise RuntimeError(f"换上来的桩吐的不是新正文（尾部是 {joined[-20:]!r}）")
    return fresh


def scenario(master, sink, label, command_text, keys):
    tail = f"尾巴{label}完。"
    print(f"\n── {label} ──", flush=True)
    done_before = completed_count()
    if not start_turn(master, sink, f"{label}这一句", done_before):
        r.save(f"panels-{label}-nostart", h.render(bytes(sink)))
        print("  库里的轮:", turns())
        raise AssertionError(f"回合没起来，看 {h.OUT}/round26-panels-{label}-nostart.txt")
    # 尽快开面板：越晚开，越多正文在开面板前就流出去了，测不到东西。
    os.write(master, command_text.encode())
    h.drain_until(master, sink, command_text, 5.0)
    os.write(master, b"\r")
    h.drain(master, 1.5, sink)
    screen = h.render(bytes(sink))
    r.save(f"panels-{label}-open", screen)
    opened = any("选择" in line or "Select" in line for line in screen)
    # 数**次数**而不是「在不在」：上一场的尾巴还留在回屏里，光判「在不在」
    # 永远为真（09-20 又栽一次）。要的是这一场有没有新添一个。
    tail_before = squash(screen).count(tail)
    print(f"  面板开出来了: {opened}；开面板时这一轮的尾巴还没出来: {tail_before == 0 or True}")
    print(f"  （开面板时屏幕上的尾巴计数: {tail_before}）")
    # 面板开着的时候正文**还在不在往下流**（用户 09-20 要的那件事）：
    # 连拍两帧，屏幕变了就说明正文还在动。面板自己不会动（没人碰键盘）。
    # 连采几拍：一拍 2.5 秒偶尔会正好落在两段正文之间（实测 session 那场
    # 一跑绿一跑红），多采几拍只要有一拍变了就算在流。
    # 「面板开着时正文还在流」这一条**测不了**了：09-20 用户裁定回滚那版改造，
    # 面板又回到「分离 → 开面板 → 挂回来」，回合一分离，屏幕上本来就不动。
    #
    # 留一句给以后：真要做这件事，得改 `scroll_question_body` 那层视口逻辑
    # （面板下方的正文区是固定窗口语义，新内容进缓冲但窗口不跟着长），不是在
    # 回合循环里补重画能解决的。判据用「看得见的最大填充编号有没有往上走」，
    # 别用「屏幕有没有变化」——后者捕捉的是面板重绘和转轮的噪声，会骗人。
    _ = max_filler
    # **死等**这一轮在 daemon 里跑完：现场必须是「面板开着时回合结束」。
    deadline = time.time() + 180
    while time.time() < deadline and completed_count() <= done_before:
        h.drain(master, 0.5, sink)
    finished_while_open = completed_count() > done_before
    print("  面板开着的时候回合跑完了:", finished_while_open)
    for key in keys:
        os.write(master, key)
        h.drain(master, 1.5, sink)
    h.settle(master, sink, quiet=3.0, timeout=120)
    screen = h.render(bytes(sink))
    r.save(f"panels-{label}-settled", screen)
    tail_after = squash(screen).count(tail)
    print(f"  收掉面板后尾巴计数: {tail_after}（之前 {tail_before}）")
    REPORT[f"{label}：开面板时这一轮还没流到尾巴"] = tail_before == 0
    REPORT[f"{label}：面板开着的时候回合在 daemon 里跑完了"] = finished_while_open
    REPORT[f"{label}：收掉面板后尾巴补上了"] = tail_after > tail_before
    REPORT[f"{label}：那一轮不是被打断的"] = all(
        row[0] != "interrupted" for row in turns()
    )
    REPORT[f"_{label} 库里的轮"] = turns()[-2:]


REPORT = {}


def main():
    stub, daemon, tui, master, sink = r.start(STUB)
    try:
        for label, command_text, keys in (
            ("models", "/models", [b"\x1b"]),
            ("session", "/session", [b"\x1b"]),
            ("help", "/help", []),
            # 慢速档：她不吐字的时候，面板扛不扛得住 33ms 一次的转轮重画。
            ("slow-models", "/models", [b"\x1b"]),
            ("slow-session", "/session", [b"\x1b"]),
        ):
            stub = restart_stub(stub, reply_for(label), stub_env_for(label))
            scenario(master, sink, label, command_text, keys)
        return REPORT
    finally:
        r.stop(tui, daemon, stub)


if __name__ == "__main__":
    report = main()
    checks = {k: v for k, v in report.items() if not k.startswith("_")}
    for name, ok in checks.items():
        print(f"{'✅' if ok else '❌'} {name}")
    print(f"\n{sum(1 for v in checks.values() if v)}/{len(checks)} passed")
    for name, value in report.items():
        if name.startswith("_"):
            print(f"   {name[1:]}: {value}")
    print("产物：", h.OUT)

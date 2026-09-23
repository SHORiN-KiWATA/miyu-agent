#!/usr/bin/env python3
"""chafa 版本 × Miyu 调用形态 的兼容矩阵。

假终端身份：xterm-256color + 应答 DA1 带 sixel(4)。
判定：sixel/kitty = 真图；symbols = 退化成字符画；退出码 2 = 图片完全不显示。
"""
import os, sys

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pty_probe as P

BASE = os.path.dirname(os.path.abspath(__file__))
OLD = os.path.join(BASE, "old")
JXL = os.path.join(OLD, "jxl/usr/lib")
IMG = P.ensure_image()

VERSIONS = [
    ("1.12.4", os.path.join(OLD, "v1.12.4")),
    ("1.14.5", os.path.join(OLD, "v1.14.5")),
    ("1.16.2", os.path.join(OLD, "v1.16.2")),
    ("1.18.0", os.path.join(OLD, "v1.18.0")),
    ("1.18.2", None),
]

CASES = [
    ("A 裸调用                       stdin=null", [], "null"),
    ("B 08-18~09-12 --probe off --relative off  stdin=null",
     ["--probe", "off", "--relative", "off"], "null"),
    ("C 09-12~09-13 --probe-mode ctty --polite on --relative off  stdin=null",
     ["--probe-mode", "ctty", "--polite", "on", "--relative", "off"], "null"),
    ("D 现在 --polite on             stdin=null", ["--polite", "on"], "null"),
    ("E --polite on                  stdin=tty", ["--polite", "on"], "tty"),
    ("F 裸调用                       stdin=tty", [], "tty"),
]


def run_one(binary, libdir, extra, stdin_mode):
    argv = [binary] + extra + ["--size", "20x10", IMG]
    backup = dict(os.environ)
    if libdir:
        os.environ["LD_LIBRARY_PATH"] = f"{libdir}:{JXL}"
    os.environ["TERM"] = "xterm-256color"
    for k in ("KITTY_WINDOW_ID", "KITTY_PID", "TERM_PROGRAM", "KITTY_INSTALLATION_DIR"):
        os.environ.pop(k, None)
    try:
        code, out = P.run(argv, stdin_mode=stdin_mode)
    except Exception as exc:  # noqa: BLE001
        return "ERR", str(exc)[:40]
    finally:
        os.environ.clear()
        os.environ.update(backup)
    if code != 0:
        text = out.replace(b"\r\n", b"\n").decode("utf-8", "replace").strip()
        last = [l for l in text.splitlines() if l.strip()]
        return f"×退出{code}", (last[-1] if last else "")[:70]
    return P.classify(out), ""


hdr = f"{'调用形态':<56}" + "".join(f"{v:>10}" for v, _ in VERSIONS)
print(hdr)
print("-" * len(hdr))
errs = {}
for label, extra, stdin_mode in CASES:
    cells = []
    for ver, dirpath in VERSIONS:
        binary = os.path.join(dirpath, "usr/bin/chafa") if dirpath else "chafa"
        libdir = os.path.join(dirpath, "usr/lib") if dirpath else None
        status, detail = run_one(binary, libdir, extra, stdin_mode)
        cells.append(f"{status:>10}")
        if detail:
            errs.setdefault(detail, set()).add(ver)
    print(f"{label:<56}" + "".join(cells))
print()
for msg, vers in errs.items():
    print(f"  [{','.join(sorted(vers))}] {msg}")

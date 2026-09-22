#!/usr/bin/env python3
"""battery-care 的 macOS 那条路:拿假的 `uname`/`pmset`/`ioreg`/`sysctl` 喂它。

**为什么要有这份东西**:手上唯一那台 Mac 是 Mac Studio,台式机,没有电池。
「没有电池」那条路在真机上验过了,**有电池的 MacBook 那条路真机上跑不了**。
与其把它一直挂着当「未验证」,不如把 `ioreg`/`pmset` 的输出照公开格式摆出来,
至少把解析那一层(一堆 sed 表达式)钉死。

说清楚它证明了什么、没证明什么:
- 证明了:解析逻辑对着这几种输出形态给得出对的数;
- **没证明**:真机上 `ioreg` 打出来的就是这几种形态。

跑法:
    python3 testkit/battery-care/macos_fake.py
"""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "src" / "scripts" / "personas" / "default" / "battery-care"

# Apple Silicon:`MaxCapacity` 是**已经算好的百分比**,而 `AppleRawMaxCapacity`
# 常常根本不在。照着 mAh 去除 DesignCapacity 会算出「健康度 2%」。
APPLE_SILICON = """
+-o AppleSmartBattery  <class AppleSmartBattery, id 0x100000abc>
    {
      "CycleCount" = 142
      "DesignCapacity" = 4790
      "MaxCapacity" = 93
      "Temperature" = 3021
    }
"""

# Intel:`AppleRawMaxCapacity` 在,单位是 mAh。
INTEL = """
+-o AppleSmartBattery  <class AppleSmartBattery, id 0x100000abc>
    {
      "CycleCount" = 512
      "DesignCapacity" = 8755
      "MaxCapacity" = 100
      "AppleRawMaxCapacity" = 7442
    }
"""

CASES = [
    {
        "name": "Apple Silicon,放电中",
        "model": "Mac16,6",
        "pmset": ("Now drawing from 'Battery Power'\n"
                  " -InternalBattery-0 (id=12582busy)\t87%; discharging;"
                  " 3:48 remaining present: true\n"),
        "ioreg": APPLE_SILICON,
        "want": ["Battery: internal", "Charge: 87%", "State: discharging",
                 "Cycles: 142", "Health: 93%", "Limit: not settable here"],
        "not_want": ["Health: 2%", "Health: 1%", "Health: 0%"],
    },
    {
        "name": "Intel,充满了",
        "model": "MacBookPro16,1",
        "pmset": ("Now drawing from 'AC Power'\n"
                  " -InternalBattery-0 (id=4653155)\t100%; charged;"
                  " 0:00 remaining present: true\n"),
        "ioreg": INTEL,
        # 7442/8755 = 85%
        "want": ["Battery: internal", "Charge: 100%", "State: charged",
                 "Cycles: 512", "Health: 85%"],
        "not_want": [],
    },
    {
        "name": "台式机,没有电池",
        "model": "Mac16,9",
        "pmset": "Now drawing from 'AC Power'\n",
        "ioreg": "",
        "want": ["Battery: none", "Reason: this Mac has no internal battery",
                 "Device: Mac16,9"],
        "not_want": ["Charge:", "Health:", "Cycles:"],
    },
]


def fake_bin(directory, model, pmset_out, ioreg_out):
    """摆一套假的系统命令。`uname -s` 说 Darwin,脚本才会走 macOS 那一支。"""
    for name, body in {
        "uname": f'#!/bin/sh\n[ "$1" = "-s" ] && echo Darwin || echo Darwin\n',
        "pmset": f'#!/bin/sh\ncat <<\'EOF\'\n{pmset_out}EOF\n',
        "ioreg": f'#!/bin/sh\ncat <<\'EOF\'\n{ioreg_out}\nEOF\n',
        "sysctl": f'#!/bin/sh\necho {model}\n',
    }.items():
        path = directory / name
        path.write_text(body)
        path.chmod(0o755)


def run(case, action):
    with tempfile.TemporaryDirectory() as tmp:
        directory = Path(tmp)
        fake_bin(directory, case["model"], case["pmset"], case["ioreg"])
        env = dict(os.environ, PATH=f"{directory}:{os.environ['PATH']}")
        return subprocess.run(["bash", str(SCRIPT), action], env=env,
                              capture_output=True, text=True, check=False)


def main():
    results = []

    def check(ok, label, detail=""):
        results.append(bool(ok))
        print(f"{'✅' if ok else '❌'} {label}{'' if ok else '  ' + detail}")

    for case in CASES:
        print(f"\n── {case['name']} ──")
        done = run(case, "status")
        check(done.returncode == 0, "status 退 0",
              f"exit={done.returncode} {done.stderr.strip()[:120]}")
        for want in case["want"]:
            check(want in done.stdout, f"给出 {want!r}",
                  f"实际:\n{done.stdout.strip()}")
        for unwanted in case["not_want"]:
            check(unwanted not in done.stdout, f"没有 {unwanted!r}",
                  f"实际:\n{done.stdout.strip()}")

    print("\n── 写那一半:要明着拒绝,不能假装改成功 ──")
    done = run(CASES[0], "set")
    check(done.returncode != 0, "set 退非 0", f"exit={done.returncode}")
    check("Support: no" in done.stdout, "说了不支持", done.stdout.strip())
    check("System Settings" in done.stdout, "指了正经去处", done.stdout.strip())

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())

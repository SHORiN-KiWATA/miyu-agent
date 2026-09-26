#!/usr/bin/env python3
"""设置界面表单去掉「保存 / 返回」两行、去掉「导航中」（用户 09-26）。

真二进制 + PTY + pyte（驱动照搬 config_visual.py），隔离的 MIYU_HOME，不起 daemon、
不发模型请求。判据：

    settings_no_navigating_label   全局设置表单导航时不写「导航中」
    settings_editing_label         打字时照旧写「编辑中」，Esc 结束编辑后它消失
    model_form_has_no_buttons      「编辑模型」表单底下没有「保存」「返回」两行
    model_form_no_navigating       「编辑模型」也不写「导航中」
    untouched_esc_changes_nothing  点开不改直接 Esc：不收（不出「已更新模型设置」）
    edited_esc_keeps_the_change    改了上下文窗口再 Esc：收下（出「已更新模型设置」）
    create_form_keeps_buttons      新增自定义模型（新增类表单）照旧有「保存」「返回」
    saved_on_exit                  主菜单「保存并退出」后，配置里是改过的上下文窗口

Run: python3 testkit/tui/config_forms.py --binary /absolute/path/to/miyu
"""

import argparse
import json
import os
import re
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import sandbox_dir  # noqa: E402
from config_visual import Driver, to_main  # noqa: E402

CONTEXT = "123456"
results = []


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + str(detail)[:300]) if detail and not ok else ''}")


def rows_of(text):
    """每一行去掉星空之后的字。"""
    return [re.sub(r"[✦✶+.]", "", line).strip() for line in (text or "").split("\n")]


def has_action_rows(text):
    """表单底下那两行动作：整行只有「保存」或「返回」，后面跟一个 ›（画出来是「保存  ›」）。"""
    rows = rows_of(text)
    return [row for row in rows if re.fullmatch(r"(›\s*)?(保存|返回)(\s*›)?", row)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    sandbox = sandbox_dir.make("miyu-config-forms-")
    home = sandbox / "home"
    (home / "config").mkdir(parents=True)
    runtime = Path(tempfile.mkdtemp(prefix="mx-cf-", dir="/tmp"))
    config_path = home / "config/config.jsonc"
    config_path.write_text(json.dumps({
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [
            {"id": "stub", "display_name": "Stub",
             "base_url": "http://127.0.0.1:1/v1", "protocol": "openai-chat",
             "api_key": "stub", "models": ["stub-model"]},
        ],
        "display": {"language": "zh"},
        "memory": {"enabled": False},
    }))

    driver = Driver(args.binary.resolve(), home, runtime)
    try:
        text = driver.wait("供应商和模型", "保存并退出")
        check(text is not None, "主菜单画出来了")

        # ── 全局设置：导航时不写「导航中」，打字时照旧「编辑中」 ──
        to_main(driver)
        text = driver.send(b"j" * 7 + b"\r", "工具最大轮数", "界面语言")
        check(text is not None and "导航中" not in text, "settings_no_navigating_label",
              "进不去全局设置" if text is None else "还写着导航中")
        text = driver.send(b"jj\r", "编辑中")
        check(text is not None, "settings_editing_label", "打字时没有「编辑中」")
        os.write(driver.master, b"\x1b")
        driver.pump(0.8)
        after = driver.text()
        check("编辑中" not in after and "导航中" not in after, "settings_editing_label_clears",
              "Esc 之后还挂着状态字")
        driver.send(b"\x1b", "供应商和模型", "保存并退出")

        # ── 编辑模型：没有「保存 / 返回」两行，Esc 改过才收 ──
        to_main(driver)
        text = driver.send(b"\r", "供应商", "组织", "模型")
        check(text is not None, "进得了供应商/模型三列")
        driver.send(b"ll", "stub-model", settle=0.4)
        text = driver.send(b"\r", "编辑模型", "上下文窗口")
        check(text is not None, "进得了编辑模型")
        text = text or driver.text()
        check(not has_action_rows(text), "model_form_has_no_buttons", has_action_rows(text))
        check("导航中" not in text, "model_form_no_navigating")
        text = driver.send(b"\x1b", "供应商", "组织", "模型")
        check(text is not None and "已更新模型设置" not in (text or ""),
              "untouched_esc_changes_nothing", "没改也收了" if text else "Esc 没回到三列")

        text = driver.send(b"\r", "编辑模型", "上下文窗口")
        check(text is not None, "再进一次编辑模型")
        driver.send(b"jj\r", "编辑中")
        os.write(driver.master, b"\x7f" * 24 + CONTEXT.encode())
        driver.pump(0.4)
        os.write(driver.master, b"\r")
        driver.pump(0.4)
        text = driver.send(b"\x1b", "已更新模型设置")
        check(text is not None, "edited_esc_keeps_the_change", driver.text()[-400:])

        # ── 新增类表单照旧有按钮 ──
        text = driver.send(b"n", "添加自定义模型")
        check(text is not None, "进得了添加自定义模型")
        text = text or driver.text()
        found = has_action_rows(text)
        check(len(found) == 2, "create_form_keeps_buttons", found)
        # 这张表单一进来就在打字：第一下 Esc 只是结束打字，第二下才是「返回」。
        os.write(driver.master, b"\x1b")
        driver.pump(0.4)
        text = driver.send(b"\x1b", "添加模型", "刷新")
        check(text is not None, "Esc 两下回到三列")

        # ── 保存并退出：配置里是改过的值 ──
        text = driver.send(b"q", "配置全局文本模型", "保存并退出")
        check(text is not None, "q 回到主菜单")
        to_main(driver)
        os.write(driver.master, b"j" * 9 + b"\r")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and driver.process.poll() is None:
            driver.pump(0.1)
        saved = config_path.read_text()
        exited = driver.process.poll() is not None
        # 模型列会把本机模型目录里的也列出来，第一行不一定是 stub-model：只认值。
        check(exited and re.search(rf'"model_context_window"\s*:\s*\{{[^}}]*:\s*{CONTEXT}\b', saved) is not None,
              "saved_on_exit", "没退出" if not exited else saved[:600])
    finally:
        driver.close()
    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed")
    return 0 if results and passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())

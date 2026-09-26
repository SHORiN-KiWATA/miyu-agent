#!/usr/bin/env python3
"""人格、Miyu 附加、用户身份的编辑等「保存并退出」才落盘（用户 09-26）。

真二进制 + PTY + pyte（驱动照搬 config_visual.py），隔离的 MIYU_HOME，不起 daemon。多行字段
走 `$EDITOR`：换成一个假编辑器，把 FAKE_EDITOR_SOURCE 那个文件的内容抄进去。

第一趟（改完选「不保存」）：
    edit_form_has_no_buttons     编辑人格表单底下没有「保存」「返回」，也没有「导航中」
    rename_shows_new_name        改了名，列表上当场是新名字
    nothing_written_before_save  改名、改正文之后盘上一个字没动
    discard_keeps_disk           退出选「不保存」：还是老名字、老正文
第二趟（改完「保存并退出」）：
    persona_renamed_on_save      盘上换成新名字、正文是改过的，老名字没了
    miyu_hint_written_on_save    Miyu 的防失忆提示写下了（保存前没写）
    identity_renamed_on_save     用户身份换成新名字、老名字没了（保存前没动）

Run: python3 testkit/tui/config_personas.py --binary /absolute/path/to/miyu
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

results = []


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + str(detail)[:300]) if detail and not ok else ''}")


def rows_of(text):
    return [re.sub(r"[✦✶+.]", "", line).strip() for line in (text or "").split("\n")]


def has_action_rows(text):
    return [row for row in rows_of(text) if re.fullmatch(r"(›\s*)?(保存|返回)(\s*›)?", row)]


class Walk:
    def __init__(self, binary, home, runtime, source):
        self.driver = Driver(binary, home, runtime)
        self.source = source
        self.home = home

    def editor_writes(self, text):
        self.source.write_text(text, encoding="utf-8")

    def send(self, keys, *words, **kw):
        return self.driver.send(keys, *words, **kw)

    def type_text(self, text):
        os.write(self.driver.master, text.encode())
        self.driver.pump(0.3)

    def find(self, name):
        return sorted(self.home.rglob(name))

    def persona_menu(self):
        to_main(self.driver)
        return self.send(b"j" * 5 + b"\r", "当前人格", "启用的功能")

    def persona_list(self):
        self.persona_menu()
        return self.send(b"\r", "Miyu", "Miyu")

    def back_to_main(self):
        """一层层 Esc 回主菜单（人格列表 → 人格和功能 → 主菜单）。"""
        for _ in range(4):
            if self.send(b"\x1b", "供应商和模型", "保存并退出", timeout=1.5, settle=0.3):
                return

    def quit(self, save):
        """回主菜单退出：save=True 走「保存并退出」，False 走 q →「不保存」。"""
        self.back_to_main()
        to_main(self.driver)
        if save:
            os.write(self.driver.master, b"j" * 9 + b"\r")
        else:
            self.send(b"q", "是否保存已编辑内容")
            os.write(self.driver.master, b"j\r")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and self.driver.process.poll() is None:
            self.driver.pump(0.1)
        exited = self.driver.process.poll() is not None
        self.driver.close()
        return exited


def rename_in_form(walk, new_name):
    """表单停在第一格「名称」：回车进编辑、退格清掉、打新名字、回车收尾。"""
    walk.send(b"\r", "编辑中")
    os.write(walk.driver.master, b"\x7f" * 30)
    walk.driver.pump(0.2)
    walk.type_text(new_name)
    os.write(walk.driver.master, b"\r")
    walk.driver.pump(0.4)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    sandbox = sandbox_dir.make("miyu-config-personas-")
    home = sandbox / "home"
    (home / "config").mkdir(parents=True)
    runtime = Path(tempfile.mkdtemp(prefix="mx-cp-", dir="/tmp"))
    (home / "config/config.jsonc").write_text(json.dumps({
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{"id": "stub", "display_name": "Stub", "base_url": "http://127.0.0.1:1/v1",
                       "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model"]}],
        "display": {"language": "zh"},
        "memory": {"enabled": False},
    }))
    source = sandbox / "editor-source.txt"
    editor = sandbox / "fake-editor.sh"
    editor.write_text(f'#!/bin/sh\ncat "{source}" > "$1"\n', encoding="utf-8")
    editor.chmod(0o755)
    os.environ["EDITOR"] = str(editor)

    # ── 第一趟：新建一个人格（新建当场写），改名改正文，退出选「不保存」 ──
    walk = Walk(binary, home, runtime, source)
    try:
        walk.driver.wait("供应商和模型", "保存并退出")
        walk.persona_list()
        walk.send(b"a", "新建人格")
        rename_in_form(walk, "角色甲")
        walk.editor_writes("原来的正文")
        os.write(walk.driver.master, b"j\r")  # 内容：多行字段，改完表单当场收下
        text = walk.send(b"", "角色甲", "Miyu") or walk.driver.text()
        created = walk.find("角色甲.md")
        check(created and created[0].read_text(encoding="utf-8").strip() == "原来的正文",
              "新建人格当场写了盘", created)

        text = walk.send(b"j\r", "编辑人格", "名称") or walk.driver.text()
        check(not has_action_rows(text) and "导航中" not in text, "edit_form_has_no_buttons",
              has_action_rows(text) or "还写着导航中")
        rename_in_form(walk, "角色乙")
        text = walk.send(b"\x1b", "角色乙", "Miyu")
        check(text is not None and "角色甲" not in (text or ""), "rename_shows_new_name",
              walk.driver.text()[-300:])
        walk.editor_writes("改过的正文")
        walk.send(b"\r", "编辑人格", "名称")
        os.write(walk.driver.master, b"j\r")
        walk.send(b"", "角色乙", "Miyu")
        on_disk = walk.find("角色甲.md")
        check(on_disk and on_disk[0].read_text(encoding="utf-8").strip() == "原来的正文"
              and not walk.find("角色乙.md"), "nothing_written_before_save",
              [str(p) for p in walk.find("角色*.md")])
        exited = walk.quit(save=False)
        still = walk.find("角色甲.md")
        check(exited and still and still[0].read_text(encoding="utf-8").strip() == "原来的正文"
              and not walk.find("角色乙.md"), "discard_keeps_disk",
              "没退出" if not exited else [str(p) for p in walk.find("角色*.md")])
    finally:
        walk.driver.close()

    # ── 第二趟：改名改正文、Miyu 附加、新建再改名一个用户身份，「保存并退出」 ──
    walk = Walk(binary, home, runtime, source)
    try:
        walk.driver.wait("供应商和模型", "保存并退出")
        walk.persona_list()
        walk.send(b"j\r", "编辑人格", "名称")
        rename_in_form(walk, "角色乙")
        walk.send(b"\x1b", "角色乙", "Miyu")
        walk.editor_writes("改过的正文")
        walk.send(b"\r", "编辑人格", "名称")
        os.write(walk.driver.master, b"j\r")
        walk.send(b"", "角色乙", "Miyu")

        # Miyu 那一行：防失忆提示（多行字段）。
        walk.editor_writes("Miyu 改过的提示")
        walk.send(b"k\r", "Miyu 人格附加")
        os.write(walk.driver.master, b"\r")
        walk.send(b"", "角色乙", "Miyu")
        miyu_hint_before = [p for p in walk.find("default.md") if p.parent.name == "hints"]

        # 用户身份：新建一个（当场写），再改名（等保存）。
        os.write(walk.driver.master, b"\x1b")
        walk.driver.pump(0.4)
        # 人格和功能菜单第 5 行是空的分隔行，j 会跳过它：5 下到「用户身份」。
        walk.send(b"j" * 5 + b"\r", "不使用用户身份")
        walk.send(b"a", "新建用户身份")
        rename_in_form(walk, "身份甲")
        walk.editor_writes("身份正文")
        os.write(walk.driver.master, b"j\r")
        walk.send(b"", "身份甲", "不使用用户身份")
        walk.send(b"j\r", "编辑用户身份", "名称")
        rename_in_form(walk, "身份乙")
        walk.send(b"\x1b", "身份乙", "不使用用户身份")
        identity_before = (bool(walk.find("身份甲.md")), bool(walk.find("身份乙.md")))
        exited = walk.quit(save=True)

        renamed = walk.find("角色乙.md")
        check(exited and renamed and renamed[0].read_text(encoding="utf-8").strip() == "改过的正文"
              and not walk.find("角色甲.md"), "persona_renamed_on_save",
              "没退出" if not exited else [str(p) for p in walk.find("角色*.md")])
        hints = [p for p in walk.find("default.md") if p.parent.name == "hints"]
        check(not miyu_hint_before and hints
              and hints[0].read_text(encoding="utf-8").strip() == "Miyu 改过的提示",
              "miyu_hint_written_on_save", {"before": miyu_hint_before, "after": hints})
        identity = walk.find("身份乙.md")
        check(identity_before == (True, False) and identity
              and identity[0].read_text(encoding="utf-8").strip() == "身份正文"
              and not walk.find("身份甲.md"), "identity_renamed_on_save",
              {"before": identity_before, "after": [str(p) for p in walk.find("身份*.md")]})
    finally:
        walk.driver.close()

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed")
    return 0 if results and passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())

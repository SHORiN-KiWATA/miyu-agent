#!/usr/bin/env python3
"""「人格」菜单与「启用的功能」表：引导之后终于有地方改 persona.toml 了。

验的是两层开关那套口径：
- 空格 = 这个人格用不用它 → 写 `persona.toml`；
- 勾上会顺手把机器层的开关也打开 → 写 `config.jsonc`；
- 回车 = 这台机器上它怎么配 → 插件设置表单；
- 两者都**跟着「保存并退出」落盘**，退出时答「不保存」就一起丢掉。

真二进制 + PTY + pyte，隔离的 MIYU_HOME，不起 daemon、不发模型请求。

Run: python3 testkit/tui/persona_menu.py --binary /absolute/path/to/miyu
"""

import argparse
import fcntl
import json
import os
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

import pyte

COLS, ROWS = 116, 50

results = []


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + detail) if detail and not ok else ''}")


class Driver:
    def __init__(self, binary, home):
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

        def setup():
            os.setsid()
            fcntl.ioctl(1, termios.TIOCSCTTY, 0)

        self.process = subprocess.Popen(
            [str(binary), "config"],
            stdin=slave, stdout=slave, stderr=slave, cwd=home,
            env=dict(os.environ, MIYU_HOME=str(home), TERM="xterm-256color",
                     COLORTERM="truecolor", LANG="zh_CN.UTF-8",
                     XDG_RUNTIME_DIR=str(home / "run")),
            preexec_fn=setup,
        )
        os.close(slave)
        self.master = master

    def pump(self, seconds=0.05):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if select.select([self.master], [], [], 0.02)[0]:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    return
                if not chunk:
                    return
                self.stream.feed(chunk)

    def text(self):
        return "\n".join(self.screen.display)

    def wait(self, *words, timeout=8.0, settle=0.35):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump(0.05)
            if all(word in self.text() for word in words):
                self.pump(settle)
                return self.text()
        return None

    def send(self, keys, *words, timeout=8.0):
        os.write(self.master, keys)
        return self.wait(*words, timeout=timeout)

    def cursor_line(self):
        """光标（`▸`）停在哪一行的文字。"""
        for line in self.screen.display:
            if "▸" in line:
                return line
        return ""

    def walk_to(self, needle, limit=60):
        """一格一格往下走，直到光标停在含 `needle` 的那一行上。"""
        for _ in range(limit):
            if needle in self.cursor_line():
                return True
            os.write(self.master, b"j")
            self.pump(0.12)
        return needle in self.cursor_line()

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        os.close(self.master)


def manifest_of(home):
    found = list(home.rglob("persona.toml"))
    return found[0].read_text() if found else ""


def counter_of(text):
    match = re.search(r"(\d+)\s*/\s*(\d+)", text)
    return (int(match.group(1)), int(match.group(2))) if match else (0, 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    sandbox = Path(tempfile.mkdtemp(prefix="miyu-persona-menu-"))
    home = sandbox / "home"
    (home / "config").mkdir(parents=True)
    (home / "run").mkdir()
    config_path = home / "config/config.jsonc"
    config_path.write_text(json.dumps({
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [{"id": "stub", "display_name": "Stub",
                       "base_url": "http://127.0.0.1:1/v1", "protocol": "openai-chat",
                       "api_key": "stub", "models": ["stub-model"]}],
        "display": {"language": "zh"},
        # 生图默认就是关的，正好用来验「勾上会顺手打开机器层」。
        "plugins": {"image_generation": {"enabled": False}},
    }))

    driver = Driver(args.binary.resolve(), home)
    try:
        text = driver.wait("供应商和模型", "保存并退出")
        check(text is not None, "主菜单画出来了")
        text = text or driver.text()
        check("人格" in text, "主菜单有「人格」这一项")
        check("插件配置" not in text and "自定义提示词" not in text,
              "「插件配置」「自定义提示词」已经并进去了")
        check("Miyu" in text, "人格那行带着当前人格的名字")

        # ── 人格菜单 ──
        text = driver.send(b"j" * 5 + b"\r", "当前人格", "启用的功能")
        check(text is not None, "进得了人格菜单")
        text = text or driver.text()
        for row in ("提示词与预设对话", "防失忆提醒", "提醒间隔", "用户身份", "开发模式提示词"):
            check(row in text, f"人格菜单里有「{row}」")
        before_counter = counter_of(
            next(line for line in text.split("\n") if "启用的功能" in line)
        )
        check(before_counter[1] > 10, "功能计数看着像回事", str(before_counter))

        # ── 功能表 ──
        text = driver.send(b"j\r", "机器能力", "内置插件")
        check(text is not None, "进得了功能表")
        text = text or driver.text()
        for section in ("机器能力", "子系统", "内置插件", "脚本"):
            check(f"── {section}" in text, f"功能表按「{section}」分节")
        check("[*]" in text, "开关是 [*]")
        check("⚙" in text, "能进设置的行摆了齿轮")
        check("(本机未开)" in text, "勾着但机器上没开的标出来了")
        check("网络搜索" in text and "长期记忆" in text and "语音功能" in text,
              "引导里藏起来的那些（机器能力/记忆/子系统）这里都摆出来了")
        # 09-23 的改名:识图→视觉识别(机器能力,只有设置面摆)、汇率→汇率查询(插件)。
        check("视觉识别" in text and "识图" not in text, "识图已改名视觉识别")
        check("汇率查询" in text, "汇率已改名汇率查询")
        # 09-23 用户拍板:读写文件/外发/人格提醒/情绪与好感度两个 scope 都不摆。
        check("读写文件" not in text and "外发" not in text
              and "人格提醒" not in text and "情绪与好感度" not in text,
              "表上不再出现读写文件/外发/人格提醒/情绪与好感度")

        # ── 取消勾选「生图」→ 人格清单写明细 ──
        check(driver.walk_to("生图"), "走得到「生图」那一行")
        os.write(driver.master, b" ")
        driver.pump(0.5)
        after = driver.text()
        check(counter_of(after)[0] == counter_of(text)[0] - 1, "空格取消勾选，计数减一",
              f"{counter_of(text)} → {counter_of(after)}")
        driver.send(b"\x1b", "当前人格", "启用的功能")
        # 2026-09-20 起功能表的改动跟着「保存并退出」走，这会儿还不该写盘。
        check(manifest_of(home) == "", "还没保存，persona.toml 先不写",
              manifest_of(home)[:80].replace("\n", " "))

        # ── 勾回来 → 白名单回到「全开」，机器层顺手打开 ──
        driver.send(b"\r", "机器能力", "内置插件")
        check(driver.walk_to("生图"), "再走到「生图」")
        os.write(driver.master, b" ")
        driver.pump(0.5)
        # 09-23:query_deepseek_status 恢复上表(用户拍板不删)。脚本节在插件节
        # 下面,walk_to 只往下走——放这儿才不会挡住后面的行程。
        check(driver.walk_to("查询 DeepSeek 状态"), "查询 DeepSeek 状态回到功能表上")
        driver.send(b"\x1b", "当前人格", "启用的功能")

        # ── 保存并退出，机器层的开关应该被打开了 ──
        driver.send(b"\x1b", "供应商和模型", "保存并退出")
        os.write(driver.master, b"j" * 9 + b"\r")
        deadline = time.monotonic() + 8
        saved = {}
        while time.monotonic() < deadline:
            driver.pump(0.2)
            try:
                saved = json.loads(re.sub(r"^\s*//.*$", "", config_path.read_text(),
                                          flags=re.M))
            except Exception:
                continue
            if saved.get("plugins", {}).get("image_generation", {}).get("enabled"):
                break
        check(saved.get("plugins", {}).get("image_generation", {}).get("enabled") is True,
              "勾上的那件，机器层的开关也开了",
              json.dumps(saved.get("plugins", {}).get("image_generation", {})))
        saved_toml = manifest_of(home)
        check(saved_toml != "", "保存之后 persona.toml 才落盘")
        check("image_generation" not in saved_toml, "勾回来了就不写明细",
              saved_toml[:100].replace("\n", " "))
        driver.close()

        # ── 第二程：改了但答「不保存」，盘上的东西一个字都不能动 ──
        driver = Driver(args.binary.resolve(), home)
        driver.wait("供应商和模型", "保存并退出")
        driver.send(b"j" * 5 + b"\r", "当前人格", "启用的功能")
        driver.send(b"j\r", "机器能力", "内置插件")
        check(driver.walk_to("闹钟"), "走到「闹钟」")
        os.write(driver.master, b" ")
        driver.pump(0.5)
        driver.send(b"\x1b", "当前人格")
        driver.send(b"\x1b", "供应商和模型", "保存并退出")
        ask = driver.send(b"q", "是否保存已编辑内容")
        check(ask is not None, "只改了功能表，退出时也会问保存不保存")
        driver.send(b"j\r")  # 「不保存」
        driver.pump(1.0)
        check(manifest_of(home) == saved_toml, "答「不保存」，persona.toml 原封不动",
              manifest_of(home)[:100].replace("\n", " "))
    finally:
        (sandbox / "last.txt").write_text(driver.text())
        driver.close()

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({sandbox})")
    raise SystemExit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""知识库设置里的 embedding 那一行：跟主页同一口径、回车跳进主页那个菜单。

09-23 用户反馈：主页写着「配置 Embedding 模型（当前：本地 · bge-small-zh…）」，
「人格和功能 → 知识库」里却说「未配置 Embedding」。那一行绑的是运行时早就不读的
旧字段 `plugins.knowledge_base.embedding_*`。

验的是：
- 那一行显示全局值（与主页一字不差），不再出现「未配置 Embedding」；
- 「语义最低分」「Embedding 超时秒数」两个死字段不摆了；
- 回车进的是主页同一个 Embedding 菜单，选完回到表单，这一行跟着变；
- 跳之前在表单里改的值不丢，保存后落盘；embedding 写进全局，旧字段一个字不碰。

真二进制 + PTY + pyte，隔离的 MIYU_HOME，不起 daemon、不发模型请求。

Run: python3 testkit/tui/kb_embedding_row.py --binary /absolute/path/to/miyu
"""

import argparse
import json
import os
import re
import tempfile
import time
from pathlib import Path

from persona_menu import Driver

results = []
LOCAL = "本地 · bge-small-zh-v1.5-int8"
REMOTE = "emb/emb-model"


def check(ok, name, detail=""):
    results.append(bool(ok))
    print(f"{'✅' if ok else '❌'} {name}{(' — ' + detail) if detail and not ok else ''}")


def row_of(text, needle):
    return next((line for line in text.split("\n") if needle in line), "")


def load(path):
    return json.loads(re.sub(r"^\s*//.*$", "", path.read_text(), flags=re.M))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    sandbox = Path(tempfile.mkdtemp(prefix="miyu-kb-embedding-row-"))
    home = sandbox / "home"
    (home / "config").mkdir(parents=True)
    (home / "run").mkdir()
    config_path = home / "config/config.jsonc"
    config_path.write_text(json.dumps({
        "active_provider": "stub",
        "active_provider_models": [{"provider_id": "stub", "model": "stub-model"}],
        "providers": [
            {"id": "stub", "display_name": "Stub", "base_url": "http://127.0.0.1:1/v1",
             "protocol": "openai-chat", "api_key": "stub", "models": ["stub-model"]},
            # 标了 embedding 模态的远程模型：Embedding 菜单里会列出来，好验「选了之后
            # 这一行跟着变」。
            {"id": "emb", "display_name": "Emb", "base_url": "http://127.0.0.1:1/v1",
             "protocol": "openai-chat", "api_key": "stub", "models": ["emb-model"],
             "model_modalities": {"emb-model": ["embedding"]}},
        ],
        "display": {"language": "zh"},
    }))

    driver = Driver(args.binary.resolve(), home)
    try:
        text = driver.wait("供应商和模型", "保存并退出")
        check(text is not None, "主菜单画出来了")
        text = text or driver.text()
        main_row = row_of(text, "配置 Embedding 模型")
        check(LOCAL in main_row, "主页那行是内置本地模型", main_row.strip())

        # ── 人格和功能 → 启用的功能 → 知识库 ──
        driver.send(b"j" * 5 + b"\r", "当前人格", "启用的功能")
        driver.send(b"j\r", "机器能力", "内置插件")
        check(driver.walk_to("知识库"), "功能表里走得到「知识库」")
        text = driver.send(b"\r", "Embedding 模型（全局）")
        check(text is not None, "回车进了知识库设置，有「Embedding 模型（全局）」这一行")
        text = text or driver.text()
        kb_row = row_of(text, "Embedding 模型（全局）")
        check(LOCAL in kb_row, "这一行显示全局值，与主页一致", kb_row.strip())
        check("未配置 Embedding" not in text, "不再出现「未配置 Embedding」")
        check("语义最低分" not in text and "Embedding 超时秒数" not in text,
              "「语义最低分」「Embedding 超时秒数」不摆了")

        # ── 跳之前先改一个别的字段：没按保存的改动回来后还在 ──
        check(driver.walk_to("搜索最大结果数"), "走到「搜索最大结果数」")
        before = row_of(driver.text(), "搜索最大结果数")
        driver.send(b"\r")
        driver.send(b"9")
        driver.send(b"\r")
        driver.pump(0.3)
        edited = row_of(driver.text(), "搜索最大结果数")
        check(re.search(r"\b59\b", edited) is not None, "搜索最大结果数改成了 59",
              f"{before.strip()} → {edited.strip()}")

        # ── 回车跳进主页同一个 Embedding 菜单 ──
        check(driver.walk_to("Embedding 模型（全局）"), "走到 embedding 那一行")
        text = driver.send(b"\r", "EMBEDDING 模型")
        check(text is not None, "回车进的是 EMBEDDING 模型菜单")
        text = text or driver.text()
        check(LOCAL in text and REMOTE in text, "菜单里有本地模型和标了 embedding 的远程模型")
        check(driver.walk_to(REMOTE), "走到远程那一项")
        text = driver.send(b"\r", "Embedding 模型（全局）")
        check(text is not None, "选完回到知识库表单")
        text = text or driver.text()
        kb_row = row_of(text, "Embedding 模型（全局）")
        check(REMOTE in kb_row, "这一行跟着变成了远程模型", kb_row.strip())
        check("▸" in kb_row, "光标停在刚才那一行上", kb_row.strip())
        check(re.search(r"\b59\b", row_of(text, "搜索最大结果数")) is not None,
              "跳转前改的 59 还在")

        # ── 保存表单，回主菜单，主页那行也变了 ──
        driver.send(b"s", "机器能力", "内置插件")
        driver.send(b"\x1b", "当前人格", "启用的功能")
        text = driver.send(b"\x1b", "供应商和模型", "保存并退出")
        text = text or driver.text()
        main_row = row_of(text, "配置 Embedding 模型")
        check(REMOTE in main_row, "主页那行同步成了远程模型", main_row.strip())

        # ── 保存并退出，看盘上的配置 ──
        check(driver.walk_to("保存并退出"), "走到「保存并退出」")
        os.write(driver.master, b"\r")
        deadline = time.monotonic() + 8
        saved = {}
        while time.monotonic() < deadline:
            driver.pump(0.2)
            try:
                saved = load(config_path)
            except Exception:
                continue
            if saved.get("embedding", {}).get("model"):
                break
        embedding = saved.get("embedding", {})
        kb = saved.get("plugins", {}).get("knowledge_base", {})
        check(embedding.get("provider_id") == "emb" and embedding.get("model") == "emb-model",
              "embedding 写进了全局 config.embedding", json.dumps(embedding))
        check(not kb.get("embedding_provider_id") and not kb.get("embedding_model"),
              "旧字段 plugins.knowledge_base.embedding_* 一个字没碰",
              json.dumps({k: kb.get(k) for k in ("embedding_provider_id", "embedding_model")}))
        check(kb.get("max_search_results") == 59, "表单里改的 59 落盘了",
              str(kb.get("max_search_results")))
    finally:
        (sandbox / "last.txt").write_text(driver.text())
        driver.close()

    passed = sum(results)
    print(f"\n{passed}/{len(results)} passed  ({sandbox})")
    raise SystemExit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()

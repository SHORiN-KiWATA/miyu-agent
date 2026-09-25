#!/usr/bin/env python3
"""真模型走查:会话中途技能目录变了,下一轮还吃不吃得到前缀缓存(09-25 指令源)。

沙箱与供应商同 `replay_live.py`(本机配置里的 `deepseek`,key 只写进临时家目录、跑完删掉)。
一个会话三轮纯文本:

    第 1 轮  沙箱里有一件全局技能 alpha
    第 2 轮  目录没变
    (往沙箱里再放一件 beta)
    第 3 轮  目录变了

每轮只有一条请求。看第 2、3 轮那条请求相对上一条:
    pure_append  前缀指纹 same >= prev(上一条请求的每条消息原样还在)
    cache_hit    cache_read 盖住了上一条请求的 prompt(差不过 256 token:末尾不满块的零头)

09-25 之前技能目录拼在 load_skill 的描述里,目录一变 tools 的字节就变,第 3 轮整段不命中;
之后描述是常量、目录随回合尾巴发,第 3 轮应当照样命中,尾巴里多一份新目录。

用法: skills_catalog_live.py <miyu 二进制> [标签]   新旧两个二进制各跑一次对比;
      逐请求记账行存到 ~/.cache/miyu-cache-replay/skills-<标签>.json
"""

import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from replay_live import Sandbox, print_table  # noqa: E402


def write_skill(home, name):
    skill = home / "extensions" / "skills" / name
    skill.mkdir(parents=True, exist_ok=True)
    (skill / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: The {name} skill for the cache walkthrough.\n---\n\nBody.\n",
        encoding="utf-8",
    )


def against_previous(rows, index):
    """第 index 条请求(从 0 数)相对它前面那一条。"""
    row, before = rows[index], rows[index - 1]
    prev, same = row.get("prev"), row.get("same")
    expected = before.get("prompt") or 0
    return {
        "pure_append": prev is not None and same is not None and same >= prev,
        "cache_hit": expected - (row.get("cache_read") or 0) <= 256,
        "prompt": row.get("prompt"),
        "cache_read": row.get("cache_read"),
        "previous_prompt": expected,
    }


def tail_catalogs(box, session_id):
    """每一轮化石尾巴里的技能目录块(没有就是空串)。"""
    catalogs = []
    for row in box.query(
        "SELECT context_messages FROM turns WHERE session_id = ? ORDER BY seq", (session_id,)
    ):
        blocks = [
            message.get("content")
            for message in json.loads(row["context_messages"] or "[]")
            if isinstance(message.get("content"), str)
            and message["content"].startswith("<available-skills")
        ]
        catalogs.append(blocks[0] if blocks else "")
    return catalogs


def scenario(box):
    write_skill(box.home, "alpha")
    box.start()
    box.ask("skills", "Reply with the single word: one.", create=True)
    session_id = box.session_id("skills")
    box.ask(session_id, "Reply with the single word: two.")
    write_skill(box.home, "beta")
    box.ask(session_id, "Reply with the single word: three.")
    rows = box.cache_rows(session_id)
    if len(rows) < 3:
        raise AssertionError(f"only {len(rows)} logged requests")
    return {
        "name": "skills",
        "second_turn": against_previous(rows, 1),
        "third_turn": against_previous(rows, 2),
        "tail_catalogs": tail_catalogs(box, session_id),
        "requests": rows,
    }


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__)
        return 2
    box = Sandbox(Path(sys.argv[1]).resolve())
    label = sys.argv[2] if len(sys.argv) > 2 else "run"
    ok = False
    try:
        result = scenario(box)
        out = Path.home() / ".cache/miyu-cache-replay" / f"skills-{label}.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(result, ensure_ascii=False, indent=1), encoding="utf-8")
        print_table(result)
        for key in ("second_turn", "third_turn"):
            print(f"  {key}: " + json.dumps(result[key], ensure_ascii=False))
        for index, catalog in enumerate(result["tail_catalogs"], 1):
            names = [part.split('"')[1] for part in catalog.split("<skill name=")[1:]]
            print(f"  第 {index} 轮尾巴里的技能目录: {names if catalog else '（无）'}")
        ok = all(result[key]["pure_append"] and result[key]["cache_hit"]
                 for key in ("second_turn", "third_turn"))
    except Exception as error:  # noqa: BLE001 — 测具中途出错也要记成失败
        print(f"❌ aborted: {error!r}")
    finally:
        box.stop()
        # 家目录里有 API key:不论成败都删。
        shutil.rmtree(box.home, ignore_errors=True)
    print("✅ both turns replay as a pure append and hit the cache" if ok
          else "❌ some turn missed the cache")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

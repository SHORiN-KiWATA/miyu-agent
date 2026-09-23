#!/usr/bin/env bash
# 模型可见面无 CJK 门禁。
#
# 检查范围刻意收窄到「模型会读到的提示性文案」：
#   - src/tools/descriptions/*.json 中键名为 description / summary /
#     stub_example 的字符串值（递归，含 parameters 嵌套）。
#   - src/personas/*/skills/**/SKILL.md 的 frontmatter `description`：它进
#     load_skill 的常驻目录，是模型面。这条按「主体必须是英文句」判，不禁中文——触发词得写成
#     用户真会说的那几个词（「去哪玩」「开播」），跟 get_exchange_rate 的
#     "USD or 美元" 是同一类功能性中文。判据：首字符是 ASCII 字母，且 CJK
#     占比不过半。frontmatter 的 display_name / summary 是人槽，豁免。
# display_name 豁免（UI 用）；enum 值、示例数据等允许中文——那是数据不是提示。
# UI 文案（i18n::text / localize）不在本门禁范围内。
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - "$@" <<'PY'
import glob
import json
import re
import sys

CJK = re.compile(r"[㐀-䶿一-鿿豈-﫿]")
CHECKED_KEYS = {"description", "summary", "stub_example"}

# 白名单:描述里的中文是功能性的,不是提示语——
#   get_exchange_rate: 示例 "USD or 美元" 告诉模型该工具接受中文货币名;
#   glob: 示例 *ai*测试* 演示模式匹配支持 CJK 文件名;
#   manage_script: "Description:/描述：" 是脚本头被逐字解析的协议标记。
#   ledger.account: 枚举账本实际默认账户名（src/ledger/books.rs），是参数数据。
ALLOWED = {
    ("get_exchange_rate.json", "$.parameters.properties.base.description"),
    ("get_exchange_rate.json", "$.parameters.properties.target.description"),
    ("glob.json", "$.parameters.properties.pattern.description"),
    ("manage_script.json", "$.parameters.properties.description.description"),
    ("ledger.json", "$.parameters.properties.account.description"),
}

def walk(node, path, hits):
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "display_name":
                continue
            if key in CHECKED_KEYS and isinstance(value, str):
                if CJK.search(value):
                    hits.append(f"{path}.{key}")
            walk(value, f"{path}.{key}", hits)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            walk(value, f"{path}[{index}]", hits)

failed = False
for file in sorted(glob.glob("src/tools/descriptions/*.json")):
    try:
        data = json.load(open(file, encoding="utf-8"))
    except Exception as error:
        print(f"INVALID JSON: {file}: {error}")
        failed = True
        continue
    hits = []
    walk(data, "$", hits)
    import os
    base = os.path.basename(file)
    hits = [hit for hit in hits if (base, hit) not in ALLOWED]
    if hits:
        failed = True
        print(f"CJK in model-facing text: {file}")
        for hit in hits:
            print(f"  {hit}")

# ── 技能 frontmatter ──────────────────────────────────────────────
SKILL_CJK_MAX = 0.5

for file in sorted(glob.glob("src/personas/*/skills/**/SKILL.md", recursive=True)):
    text = open(file, encoding="utf-8").read()
    matched = re.search(r"^description:\s*(.*)$", text, re.M)
    if not matched:
        print(f"skill without a description: {file}")
        failed = True
        continue
    description = matched.group(1).strip()
    # 未加引号的标量里出现 ": " 会被 YAML 当成嵌套映射,整个 frontmatter 解析
    # 失败——这份技能就静默加载不到(扫描逐目录跳过),09-21 实测踩过。
    if ": " in description and not description.startswith(("'", '"')):
        print(f"skill description has an unquoted colon (breaks YAML): {file}")
        failed = True
        continue
    if not description[:1].isascii() or not description[:1].isalpha():
        print(f"skill description must open with an English sentence: {file}")
        print(f"  {description[:60]}")
        failed = True
        continue
    ratio = len(CJK.findall(description)) / max(len(description), 1)
    if ratio > SKILL_CJK_MAX:
        print(f"skill description is mostly CJK ({ratio:.0%}): {file}")
        print("  keep the sentence English; Chinese is for trigger words only")
        failed = True

if failed:
    sys.exit(1)
print("model-facing descriptions: English-led, all JSON valid")
PY

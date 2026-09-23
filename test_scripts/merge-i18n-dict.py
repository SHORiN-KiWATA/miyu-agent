#!/usr/bin/env python3
"""把 `.i18n-parts/*.json` 分片合并成 `web/i18n-en.js`(2026-09-23)。

铺量时每个文件先写自己的分片(并行施工互不打架),这里统一生成最终词典:
按中文键排序、JSON 转义、冲突(同一键两种译法)当场报错。生成后分片目录
可以整份删掉——词典的唯一真相源是 web/i18n-en.js。

    python3 test_scripts/merge-i18n-dict.py            # 合并并写文件
    python3 test_scripts/merge-i18n-dict.py --check    # 只检查冲突,不写
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PARTS = ROOT / ".i18n-parts"
DICT = ROOT / "web" / "i18n-en.js"

HEADER = """// WebUI 英文词典(2026-09-23):键 = 中文原文,值 = 英文。与 web/i18n.js 配对。
// 中文界面下服务端只发一个空壳(见 crates/miyu-hosts/src/web/assets.rs),
// 这份内容不进中文用户的下载。
//
// 维护:新增/修改界面文案时,把中文写进代码里的 t("…")/data-i18n,英文补进
// 本文件;scripts/check-webui-i18n.py 会把漏掉的揪出来(缺词条=红)。
// 本文件由 test_scripts/merge-i18n-dict.py 从分片生成,勿手改条目顺序。
window.MIYU_I18N_EN = Object.freeze({
"""


def main() -> int:
    parts = sorted(PARTS.glob("*.json"))
    if not parts and "--check" not in sys.argv:
        # 分片目录是铺量期的中间产物,铺完就删了;之后 web/i18n-en.js 本身是
        # 唯一真相源(手改条目就行)。这里必须拦住——否则"合并"会把词典写空。
        print(
            "✗ 没有分片可合并(.i18n-parts/*.json 不存在)。\n"
            "  铺量期结束后词典的唯一真相源是 web/i18n-en.js,直接改它即可;\n"
            "  确实要重建分片的话,先按文件名重建 .i18n-parts/<file>.json 再跑。"
        )
        return 1
    merged: dict[str, str] = {}
    origin: dict[str, str] = {}
    conflicts = []
    for part in parts:
        data = json.loads(part.read_text(encoding="utf-8"))
        for key, value in data.items():
            if key in merged and merged[key] != value:
                conflicts.append((key, origin[key], merged[key], part.name, value))
                continue
            merged[key] = value
            origin.setdefault(key, part.name)
    if conflicts:
        print("✗ 同键不同译法,先统一口径:")
        for key, first_part, first, part, value in conflicts:
            print(f"  {key!r}\n    {first_part}: {first!r}\n    {part}: {value!r}")
        return 1
    if "--check" in sys.argv:
        print(f"✓ 无冲突,{len(merged)} 条")
        return 0
    body = "".join(
        f"  {json.dumps(key, ensure_ascii=False)}: {json.dumps(value, ensure_ascii=False)},\n"
        for key, value in sorted(merged.items())
    )
    DICT.write_text(HEADER + body + "});\n", encoding="utf-8")
    print(f"✓ 写入 {DICT.relative_to(ROOT)}:{len(merged)} 条(来自 {len(parts)} 个分片)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

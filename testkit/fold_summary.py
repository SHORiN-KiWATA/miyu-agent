"""过程收缩行（`› …`）与回合收尾行（`✻ …`）长什么样——走查认它们统一走这里。

09-26 起收缩行不再有 `Worked for`、不挂耗时（终端 `summary_line` 与网页 `procLineRefresh`
同一套），一律英文、不随界面语言变：

    Ran 3 commands · 2 edits · 4 tools · 1 thought · 1 err
    Thought for 5.2s                                                  （只想了想）

09-26 当天有过一版中文「运行了 3 次命令 · 编辑了 2 次 …」，正则里还认它，好拿那版二进制做对照。

一轮花了多久挪到回复末尾那行：`✻ 模型 · 处理了 3 分 14 秒 · 1:53 完成`（被打断的写
「中断」，英文 `done at` / `stopped at`）。

老二进制（09-24 那套 `Worked for 12s · …` / `Ran 3 commands · … · 12s`）也认，好拿它做
修复前的对照。
"""
import re

FOLD_SUMMARY_RE = re.compile(
    r"Worked for \S|Ran \d+ commands?\b|Thought for \S|Took \S"
    r"|[›⌄] \d+ (?:edits?|tools?|thoughts?)\b"
    r"|运行了 \d+ 次命令|编辑了 \d+ 次|用了 \d+ 个工具|思考了(?: \S|不到)|出错了 \d+ 次|用时(?: \S|不到)"
)

# `✻ [模型 · ]<动词> <用时> · <几点> 完成|中断`（英文 `done at` / `stopped at`）。
TURN_END_RE = re.compile(r"✻ .+ · .+ (?:完成|中断)$|✻ .+ · (?:done|stopped) at .+$")


def is_fold_summary(line: str) -> bool:
    return bool(FOLD_SUMMARY_RE.search(line))


def is_turn_end(line: str) -> bool:
    return bool(TURN_END_RE.search(line.strip()))


def turn_end_seconds(line: str):
    """`✻` 那行报的用时（秒）；认不出返回 None。"""
    text = line.strip()
    zh = re.search(r"(?:(\d+) 小时 )?(?:(\d+) 分 )?(\d+) 秒", text)
    if zh:
        hours, minutes, seconds = (int(part or 0) for part in zh.groups())
        return hours * 3600 + minutes * 60 + seconds
    en = re.search(r"(?:(\d+)h )?(?:(\d+)m )?(\d+)s\b", text)
    if en:
        hours, minutes, seconds = (int(part or 0) for part in en.groups())
        return hours * 3600 + minutes * 60 + seconds
    return None

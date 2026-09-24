"""过程收缩行（`› …`）的摘要长什么样——走查认收缩行统一走这里。

09-24 起摘要按工具类别写（终端 `summary_line` 与网页 `procLineRefresh` 同一套）：
跑过命令 `Ran 3 commands · 2 edits · 4 tools · 1 thought · 1 err · 12s`，没跑命令
`Worked for 12s · 2 edits · 4 tools`，只想了想 `Thought for 5s`。以前走查只认
`Worked for`，跑过命令的那一段就认不出来了。

回放历史没有计时、又没跑命令时，摘要只剩计数（`2 tools · 1 thought`），这里认不出——
和以前只认 `Worked for` 时一样，需要的走查自己按计数找。
"""
import re

FOLD_SUMMARY_RE = re.compile(r"Worked for \S|Ran \d+ commands?\b|Thought for \S")


def is_fold_summary(line: str) -> bool:
    return bool(FOLD_SUMMARY_RE.search(line))

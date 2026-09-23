#!/usr/bin/env python3
"""WebUI 双语门禁(2026-09-23,配合 web/i18n.js)。

两件事:
  1. 残留检查:web/ 的 JS/HTML 里任何「给用户看的中文字符串」都必须包在
     t("…") 里,或在 HTML 上挂 data-i18n*(启动时由 i18n.js 扫一遍)。漏包的
     字符串=英文用户看到中文,红。
  2. 词典完整性:所有 t("…")/data-i18n 收集到的中文键都必须出现在
     web/i18n-en.js,漏词条=回退中文,红。

判定细节:
  - 注释、正则字面量不算(注释不进界面;正则里的中文是在匹配数据,例如
    剥中文标点的 /[，。！]/)。
  - 模板串含 ${} 且含中文 = 直接红:插值后的文本没法当词典键,必须改写成
     t("…{name}…", {name})。
  - 少数「是数据不是文案」的中文(接口字段值、按中文原文查表的数据结构等)
     在行尾写 `// i18n-allow: 理由` 放行——理由必填,写清楚为什么它不是文案。

用法:
    python3 test_scripts/check-webui-i18n.py            # 全量
    python3 test_scripts/check-webui-i18n.py web/app.js # 单文件
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WEB = ROOT / "web"
DICT_FILE = WEB / "i18n-en.js"

# 只认汉字本身:全角符号(＋ · ！)是图形/标点,不是要翻译的文案。
HAN = re.compile(r"[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]")
# t("…") 的调用点:裸 t 或 MiyuI18n.t(前面的字符不能是标识符/属性访问)
T_CALL = re.compile(r"(?<![\w$.])(?:MiyuI18n\s*\.\s*)?t\s*\(")


# 关键字后面跟 `/` 也是正则(`return /re/`、`case /re/`),不能按除法算——
# 2026-09-23 app.js 的 `return /^\s*```/` 就是这么把正则里的反引号当成模板串
# 起点、把后面整段代码错扫成"没包的字符串"的。
REGEX_KEYWORDS = {
    "return", "typeof", "instanceof", "in", "of", "case", "do", "else", "yield",
    "await", "delete", "void", "new", "throw",
}


def starts_regex(code, index):
    """index 处的 `/` 是不是正则字面量的开头。"""
    prev = ""
    for ch in reversed(code[:index]):
        if not ch.isspace():
            prev = ch
            break
    if prev == "" or prev in "(,=:[!&|?{};+-*%~^<>":
        return True
    # 退化成标识符的情况:往前取一整段标识符,是关键字就当正则
    j = index - 1
    while j >= 0 and code[j].isspace():
        j -= 1
    end = j + 1
    while j >= 0 and (code[j].isalnum() or code[j] in "_$"):
        j -= 1
    return code[j + 1 : end] in REGEX_KEYWORDS


def string_end(src, start):
    """普通字符串(start 是引号)结束后的位置。"""
    quote = src[start]
    i = start + 1
    n = len(src)
    while i < n:
        if src[i] == "\\":
            i += 2
            continue
        if src[i] == quote:
            return i + 1
        i += 1
    return n


def regex_end(src, start):
    """正则字面量(start 是 `/`)结束后的位置;字符类里的 `/` 不算结束。"""
    i = start + 1
    n = len(src)
    in_class = False
    while i < n:
        c = src[i]
        if c == "\\":
            i += 2
            continue
        if c == "[":
            in_class = True
        elif c == "]":
            in_class = False
        elif c == "/" and not in_class:
            return i + 1
        elif c == "\n":
            return i
        i += 1
    return n


def template_end(src, start):
    r"""模板字面量(start 是反引号)结束后的位置。

    `${}` 里可以再套字符串/正则/模板串(`` `a${`b${c}`}` `` 这种),不处理的话
    第一个反引号就被当成结尾,后面整段代码会被吞掉——2026-09-23 app.js 的嵌套
    模板就让闸少报了一百多处。
    """
    i = start + 1
    n = len(src)
    while i < n:
        c = src[i]
        if c == "\\":
            i += 2
            continue
        if c == "`":
            return i + 1
        if c == "$" and i + 1 < n and src[i + 1] == "{":
            i = interpolation_end(src, i + 2)
            continue
        i += 1
    return n


def interpolation_end(src, start):
    """模板串 `${` 之后(start 在 `{` 后一位)到配对 `}` 之后的位置。"""
    depth = 1
    i = start
    n = len(src)
    while i < n:
        c = src[i]
        if c in "\"'":
            i = string_end(src, i)
            continue
        if c == "`":
            i = template_end(src, i)
            continue
        if c == "/" and not src.startswith("//", i) and not src.startswith("/*", i):
            if starts_regex(src, i):
                i = regex_end(src, i)
                continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def strip_js_comments(src):
    """去注释,但**逐字符保长**(注释字符换成空格,换行原样保留):偏移量与
    原文件一一对应,报的行号和 i18n-allow 行尾标记才不会错位。"""
    out = []
    i, n = 0, len(src)
    state = None  # None | "'" | '"' | '`' | '//' | '/*' | '/re'
    while i < n:
        c = src[i]
        if state is None:
            if src.startswith("//", i):
                state = "//"
                out.append("  ")
                i += 2
                continue
            if src.startswith("/*", i):
                state = "/*"
                out.append("  ")
                i += 2
                continue
            if c == "`":
                end = template_end(src, i)
                out.append(src[i:end])
                i = end
                continue
            if c in "\"'":
                state = c
                out.append(c)
                i += 1
                continue
            # 正则字面量:运算符位或关键字之后才可能是正则(见 starts_regex)
            if c == "/" and starts_regex(src, i):
                end = regex_end(src, i)
                out.append(src[i:end])
                i = end
                continue
            out.append(c)
            i += 1
        elif state == "//":
            if c == "\n":
                state = None
                out.append(c)
            else:
                out.append(" ")
            i += 1
        elif state == "/*":
            if src.startswith("*/", i):
                state = None
                out.append("  ")
                i += 2
                continue
            if c == "\n":
                out.append(c)
            else:
                out.append(" ")
            i += 1
        else:  # 字符串/模板串
            if c == "\\":
                out.append(src[i : i + 2])
                i += 2
                continue
            if c == state:
                state = None
            out.append(c)
            i += 1
    return "".join(out)


def strip_html_comments(src):
    return re.sub(r"<!--.*?-->", lambda m: re.sub(r"[^\n]", " ", m.group(0)), src, flags=re.S)


def js_unescape(text):
    """把 JS 字符串字面量的内容解码成实际字符(转义写法差异不影响词条匹配)。"""
    out = []
    i = 0
    simple = {
        "n": "\n", "t": "\t", "r": "\r", "b": "\b", "f": "\f", "v": "\v",
        "0": "\0", "'": "'", '"': '"', "\\": "\\", "`": "`", "/": "/",
    }
    while i < len(text):
        c = text[i]
        if c != "\\" or i + 1 >= len(text):
            out.append(c)
            i += 1
            continue
        nxt = text[i + 1]
        if nxt in simple:
            out.append(simple[nxt])
            i += 2
        elif nxt == "u" and i + 5 < len(text) + 1 and len(text) >= i + 6:
            try:
                out.append(chr(int(text[i + 2 : i + 6], 16)))
                i += 6
            except ValueError:
                out.append(nxt)
                i += 2
        elif nxt == "x" and len(text) >= i + 4:
            try:
                out.append(chr(int(text[i + 2 : i + 4], 16)))
                i += 4
            except ValueError:
                out.append(nxt)
                i += 2
        else:
            out.append(nxt)
            i += 2
    return "".join(out)


def t_call_spans(code):
    """每个 t(…) 调用的第一个实参区间 [(start, end, text_of_call_line)]。"""
    spans = []
    for match in T_CALL.finditer(code):
        i = match.end()
        depth = 1
        arg_start = i
        while i < len(code) and depth:
            c = code[i]
            if c in "\"'`":
                quote = c
                i += 1
                while i < len(code):
                    if code[i] == "\\":
                        i += 2
                        continue
                    if code[i] == quote:
                        break
                    i += 1
            elif c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    break
            elif c == "," and depth == 1:
                break
            i += 1
        spans.append((arg_start, i, match.start()))
    return spans


def find_literals(code):
    r"""字符串/模板字面量 [(start, end, raw)]。正则字面量要跳过——不跳的话字符类
    里的引号/反引号会让扫描错位(2026-09-23 dash-memes 那个剥标点的字符类就把后
    面的代码整段错当成字符串)。判据与 strip_js_comments 同一套:前一个非空字符
    是运算符位才可能是正则。"""
    out = []
    i, n = 0, len(code)
    while i < n:
        c = code[i]
        if c == "/" and not code.startswith("//", i) and not code.startswith("/*", i):
            if starts_regex(code, i):
                i = regex_end(code, i)
                continue
        if c == "`":
            start = i
            i = template_end(code, i)
            out.append((start, i, code[start:i]))
            continue
        if c in "\"'":
            start = i
            i = string_end(code, i)
            out.append((start, i, code[start:i]))
        else:
            i += 1
    return out


def line_of(src, offset):
    return src.count("\n", 0, offset) + 1


def line_text(src, offset):
    start = src.rfind("\n", 0, offset) + 1
    end = src.find("\n", offset)
    return src[start : end if end != -1 else len(src)]


def allow_marked(src, offset, raw):
    """`i18n-allow: 理由` 写在该行行尾;多行模板串(HTML 骨架)允许写在**上一行**
    ——首行行尾加注释会落进模板内容里。

    「上一行」只对**跨行模板字面量**生效:否则 allow 行下面那一行里的普通
    字面量会被连带放过(既不报残留、也不进缺词条检查,等于静默漏译——
    2026-09-23 dash-affection 的 EMO_SOURCE 就是这么漏的)。"""
    if "i18n-allow" in line_text(src, offset):
        return True
    if not (raw.startswith("`") and "\n" in raw):
        return False
    line_start = src.rfind("\n", 0, offset)
    if line_start <= 0:
        return False
    return "i18n-allow" in line_text(src, line_start - 1)


def check_js(path):
    src = path.read_text(encoding="utf-8")
    code = strip_js_comments(src)
    spans = t_call_spans(code)
    problems = []
    keys = set()
    for start, end, call_at in find_literals(code):
        raw = code[start:end]
        if not HAN.search(raw):
            continue
        line = line_of(src, start)
        if allow_marked(src, start, raw):
            continue
        allowed = any(s <= start < e for s, e, _ in spans)
        if allowed:
            if raw.startswith("`") and "${" in raw:
                problems.append((line, "模板串带插值,没法当词典键;改成 t(\"…{x}…\", {x})", raw[:50]))
                continue
            if start == next((s for s, e, _ in spans if s <= start < e), -1):
                keys.add(js_unescape(raw[1:-1]))
            continue
        problems.append((line, "中文字符串没包 t()", raw[:50]))
    return problems, keys


HTML_ATTRS_TO_MARK = {"title", "aria-label", "placeholder", "alt", "data-prompt"}


def check_html(path):
    """用真正的 HTML 解析器审 index.html:
    - 带汉字的 title/aria-label/placeholder/alt/data-prompt 必须配对应的
      data-i18n-* 标记;
    - 带汉字的文本节点必须落在挂了 data-i18n / data-i18n-text 的元素里;
    - data-i18n 元素不能有子元素(它是整块 textContent,会把子元素擦掉)。
    """
    from html.parser import HTMLParser

    src = path.read_text(encoding="utf-8")
    lines = src.split("\n")
    problems = []
    keys = set()
    void = {"area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"}

    class Audit(HTMLParser):
        def __init__(self):
            super().__init__(convert_charrefs=True)
            self.stack = []
            self.problems = []
            self.keys = set()
            self.skip = 0

        def marked_attrs(self, attrs):
            return {name for name, _ in attrs if name.startswith("data-i18n")}

        def handle_starttag(self, tag, attrs):
            if tag in ("script", "style"):
                self.skip += 1
            values = dict(attrs)
            marked = self.marked_attrs(attrs)
            for name, value in attrs:
                if not value or not HAN.search(value):
                    continue
                if name.startswith("data-i18n"):
                    # data-i18n-text 是标记,真正的键是它接管的文本节点(下面 handle_data 收)
                    self.keys.add(value.strip())
                    continue
                if name in HTML_ATTRS_TO_MARK and f"data-i18n-{name.replace('data-', '')}" in marked:
                    self.keys.add(value)
                    continue
                if name == "lang":
                    continue
                self.problems.append((self.getpos()[0], f"属性 {name} 没挂 data-i18n*", value[:50]))
            if tag not in void:
                if self.stack:
                    self.stack[-1]["children"] += 1
                self.stack.append(
                    {
                        "tag": tag,
                        "line": self.getpos()[0],
                        "i18n": "data-i18n" in values,
                        "i18n_value": values.get("data-i18n", ""),
                        "text": "data-i18n-text" in values,
                        "children": 0,
                        "text_han": 0,
                    }
                )

        def handle_startendtag(self, tag, attrs):
            self.handle_starttag(tag, attrs)
            if tag not in void:
                self.handle_endtag(tag)

        def handle_endtag(self, tag):
            if tag in ("script", "style"):
                self.skip -= 1
            for index in range(len(self.stack) - 1, -1, -1):
                if self.stack[index]["tag"] == tag:
                    frame = self.stack[index]
                    self.stack = self.stack[:index]
                    if frame["i18n"] and frame["children"]:
                        self.problems.append(
                            (frame["line"], "data-i18n 元素里有子元素(会被子元素一起擦掉)", frame["tag"])
                        )
                    if frame["i18n"] and HAN.search(frame["i18n_value"]) and not frame["text_han"]:
                        self.problems.append(
                            (frame["line"], "data-i18n 元素里没有汉字文本(标记放错元素了?)", frame["tag"])
                        )
                    break

        def handle_data(self, data):
            if self.skip:
                return
            if not HAN.search(data):
                return
            line = self.getpos()[0]
            # 从最内层往外找第一个挂标记的祖先:它就是接管这段文本的元素
            owner = next(
                (frame for frame in reversed(self.stack) if frame["i18n"] or frame["text"]), None
            )
            if owner is not None:
                owner["text_han"] += 1
                # data-i18n-text 是逐个文本节点翻译的,每个文本节点都要有词条
                if owner["text"]:
                    self.keys.add(data.strip())
                return
            # 行尾 i18n-allow 放行(与 JS 侧同一约定)
            text_line = lines[line - 1] if 0 < line <= len(lines) else ""
            if "i18n-allow" in text_line:
                return
            self.problems.append((line, "HTML 文本没挂 data-i18n", data.strip()[:50]))

    audit = Audit()
    audit.feed(src)
    audit.close()
    # 未闭合元素在 close 前也校验一遍
    for frame in audit.stack:
        if frame["i18n"] and frame["children"]:
            audit.problems.append(
                (frame["line"], "data-i18n 元素里有子元素(会被子元素一起擦掉)", frame["tag"])
            )
    return audit.problems, audit.keys


def dict_entries():
    """词典条目 = web/i18n-en.js + 在途的 .i18n-parts/*.json(并行铺量时各文件
    先写自己的分片,合并后删除分片目录)。"""
    import json

    entries = set(
        js_unescape(key)
        for key in re.findall(
            r'^\s*"((?:[^"\\]|\\.)*)"\s*:', DICT_FILE.read_text(encoding="utf-8"), re.M
        )
    )
    parts = ROOT / ".i18n-parts"
    if parts.is_dir():
        for part in sorted(parts.glob("*.json")):
            try:
                entries |= set(json.loads(part.read_text(encoding="utf-8")).keys())
            except Exception as error:  # 分片写坏了要当场炸,别静默少算
                print(f"分片解析失败 {part}: {error}")
    return entries


def main():
    args = sys.argv[1:]
    targets = [Path(a) if Path(a).is_absolute() else ROOT / a for a in args]
    if not targets:
        targets = sorted(WEB.glob("*.js")) + [WEB / "index.html"]
    targets = [t for t in targets if t.name not in {"i18n.js", "i18n-en.js"}]

    entries = dict_entries()
    all_keys = set()
    failed = False
    violations = 0
    for path in targets:
        if path.suffix == ".html":
            problems, keys = check_html(path)
        else:
            problems, keys = check_js(path)
        all_keys |= keys
        for line, why, sample in problems:
            failed = True
            violations += 1
            print(f"{path.relative_to(ROOT)}:{line}: {why}  →  {sample}")
    missing = sorted(k for k in all_keys if k not in entries)
    for key in missing:
        failed = True
        violations += 1
        print(f"web/i18n-en.js 缺词条: {key}")
    if failed:
        print(f"\n✗ 未通过:残留 {violations} 处")
        return 1
    print(f"✓ {len(targets)} 个文件通过;引用中文键 {len(all_keys)} 条,词典 {len(entries)} 条")
    return 0


if __name__ == "__main__":
    sys.exit(main())

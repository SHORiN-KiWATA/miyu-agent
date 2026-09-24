"""真供应商吐工具调用的节奏:工具名先到、参数慢慢流,还是一口气全给?

「准备编辑」只在「工具名已解码、参数还在流」那段窗口里显示(09-24 用户:
「有的时候 AI 在准备编辑文件,但是我看不见准备编辑这个 tag 行」)。窗口有多长
取决于供应商怎么分片——这里直接打端点,把每个 SSE 分片的到达时刻记下来。

用法:
    python3 tool_stream_timing.py [供应商 id] [模型]     # 默认 opencodego deepseek-v4.1-flash

从 `$MIYU_HOME/config/config.jsonc`(默认 ~/.miyu)取 key 与地址。opencode 那两条
(Zen / Go)带上 Miyu 发的那几个头(见 zen_headers.rs),工具面里也照 zen_tools.rs
补上 shell + read。只读、不落盘,key 不打印。
"""

import json
import os
import pathlib
import re
import sys
import time
import urllib.request

PROVIDER = sys.argv[1] if len(sys.argv) > 1 else "opencodego"
MODEL = sys.argv[2] if len(sys.argv) > 2 else "deepseek-v4.1-flash"
UA = "opencode/1.18.29 ai-sdk/provider-utils/4.0.46 runtime/bun/1.4.0"


def load_provider():
    home = pathlib.Path(os.environ.get("MIYU_HOME", pathlib.Path.home() / ".miyu"))
    raw = (home / "config" / "config.jsonc").read_text(encoding="utf-8")
    config = json.loads(re.sub(r"^\s*//.*$", "", raw, flags=re.M))
    for provider in config.get("providers", []):
        if provider.get("id") == PROVIDER:
            return provider
    raise SystemExit(f"config 里没有供应商 {PROVIDER}")


def tool(name, description, properties):
    return {"type": "function", "function": {
        "name": name, "description": description,
        "parameters": {"type": "object", "properties": properties, "required": list(properties)}}}


def main():
    provider = load_provider()
    base = provider["base_url"].rstrip("/")
    key = provider.get("api_key") or ""
    headers = {"Content-Type": "application/json", "Authorization": f"Bearer {key}"}
    if "opencode.ai/zen" in base:
        headers.update({
            "User-Agent": UA, "x-opencode-client": "cli", "x-opencode-project": "global",
            "x-opencode-session": "ses_tooltiming0000000000000", "x-opencode-request": "msg_tooltiming000000000000",
        })
    tools = [
        tool("edit", "Apply a patch to files. patchText uses *** Begin Patch / *** Add File: <path> / +line / *** End Patch.",
             {"patchText": {"type": "string"}}),
        tool("shell", "Run a shell command.", {"command": {"type": "string"}}),
        tool("read", "Read a file.", {"path": {"type": "string"}}),
    ]
    body = json.dumps({
        "model": MODEL,
        "stream": True,
        "messages": [{"role": "user", "content":
                      "Use the edit tool to create /tmp/story.md with a 30-line story opening, one sentence per line. "
                      "Call the tool directly, no explanation."}],
        "tools": tools,
    }).encode()
    request = urllib.request.Request(f"{base}/chat/completions", data=body, headers=headers)
    started = time.time()
    events = []
    with urllib.request.urlopen(request, timeout=180) as response:
        for raw in response:
            line = raw.decode("utf-8", "replace").strip()
            if not line.startswith("data:"):
                continue
            data = line[5:].strip()
            if data == "[DONE]":
                break
            try:
                chunk = json.loads(data)
            except json.JSONDecodeError:
                continue
            at = round(time.time() - started, 3)
            for choice in chunk.get("choices") or []:
                delta = choice.get("delta") or {}
                for call in delta.get("tool_calls") or []:
                    function = call.get("function") or {}
                    events.append((at, function.get("name"), len(function.get("arguments") or "")))
                if delta.get("content"):
                    events.append((at, "<content>", len(delta["content"])))
                if delta.get("reasoning_content") or delta.get("reasoning"):
                    events.append((at, "<reasoning>", 0))
    calls = [e for e in events if e[1] not in ("<content>", "<reasoning>")]
    named = [e for e in calls if e[1]]
    args = [e for e in calls if e[2]]
    reasoning = [e for e in events if e[1] == "<reasoning>"]
    print(f"{PROVIDER} / {MODEL}")
    if reasoning:
        print(f"  思考分片 {len(reasoning)} 个:{reasoning[0][0]}s → {reasoning[-1][0]}s")
        # 思考中途最长停多久:「思考停了就改口」的阈值得比它长,不然想到一半就误报。
        gaps = [round(b[0] - a[0], 3) for a, b in zip(reasoning, reasoning[1:])]
        print(f"  思考中途最长停顿 {max(gaps, default=0)}s")
    if not calls:
        print("  没有工具调用分片(模型没调工具?)")
        return
    print(f"  工具调用分片 {len(calls)} 个;带名字的 {len(named)} 个;带参数的 {len(args)} 个")
    print(f"  名字首次出现 {named[0][0] if named else None}s;参数 {args[0][0] if args else None}s → {args[-1][0] if args else None}s;"
          f"参数共 {sum(e[2] for e in args)} 字符")
    window = (args[-1][0] - named[0][0]) if named and args else None
    print(f"  准备窗口(名字到参数流完)≈ {window}s")
    before = [e for e in events if e[0] < calls[0][0] and e[1] in ("<content>", "<reasoning>")]
    if before:
        # 思考/正文停了之后多久才来第一个工具分片:这段静默里模型其实在写参数,只是上游没往外吐。
        print(f"  上一段思考/正文结束 {before[-1][0]}s → 第一个工具分片 {calls[0][0]}s,静默 {round(calls[0][0] - before[-1][0], 3)}s")
    total = sum(e[2] for e in args)
    if window:
        print(f"  参数到达速率 ≈ {round(total / window)} 字符/秒")
    print("  前 6 个工具分片:", calls[:6])


if __name__ == "__main__":
    main()

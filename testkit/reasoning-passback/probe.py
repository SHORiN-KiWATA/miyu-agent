#!/usr/bin/env python3
"""思考回传探针:网关收不收历史里 assistant 工具调用轮带的 `reasoning_content`。

    python3 testkit/reasoning-passback/probe.py <供应商 id> <模型> [<供应商 id> <模型> ...]

读本机配置(默认 ~/.miyu/config/config.jsonc,`MIYU_CONFIG` 可改)里该供应商的
base_url 与 key,照 Miyu 的形状发两次最小请求:一轮工具调用 + 工具结果,之后让
模型接着说。A 不带 `reasoning_content` 键(不在白名单上的现状),B 带上。

看三样:HTTP 状态、流里有没有 error、B 的 prompt_tokens 是不是比 A 多——多了
说明网关把思考真的转给了模型,一样多说明收下后丢了。**不打印 key 与请求头。**
打的是真实端点,每次几百 token。
"""
import json
import os
import random
import re
import string
import sys
import urllib.error
import urllib.request
from pathlib import Path

CONFIG = Path(os.environ.get("MIYU_CONFIG", "~/.miyu/config/config.jsonc")).expanduser()
# 与 crates/miyu-core/src/llm/openai_compatible/zen_headers.rs 同一套识别头。
ZEN_ROOT = "https://opencode.ai/zen"
ZEN_UA = "opencode/1.18.29 ai-sdk/provider-utils/4.0.46 runtime/bun/1.4.0"
REASONING = "The user wants 17*23. I will call the calculator instead of guessing."


def strip_jsonc(raw):
    out, i, n, in_str = [], 0, len(raw), False
    while i < n:
        c = raw[i]
        if in_str:
            out.append(c)
            if c == "\\":
                out.append(raw[i + 1])
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if c == '"':
            in_str = True
            out.append(c)
            i += 1
            continue
        if raw.startswith("//", i):
            j = raw.find("\n", i)
            i = n if j < 0 else j
            continue
        if raw.startswith("/*", i):
            j = raw.find("*/", i)
            i = n if j < 0 else j + 2
            continue
        out.append(c)
        i += 1
    return re.sub(r",(\s*[}\]])", r"\1", "".join(out))


def provider(config, provider_id):
    for item in config.get("providers", []):
        if item.get("id") == provider_id:
            return item
    raise SystemExit(f"配置里没有供应商 {provider_id}")


def suffix():
    return "".join(random.choice(string.ascii_letters + string.digits) for _ in range(26))


def body(model, with_reasoning):
    assistant = {
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": "call_probe_1", "type": "function",
            "function": {"name": "calculator", "arguments": json.dumps({"expression": "17*23"})},
        }],
    }
    if with_reasoning:
        assistant["reasoning_content"] = REASONING
    return {
        "model": model,
        "stream": True,
        "stream_options": {"include_usage": True},
        "max_tokens": 256,
        "messages": [
            {"role": "system", "content": "You are a concise assistant."},
            {"role": "user", "content": "What is 17*23? Use the calculator tool."},
            assistant,
            {"role": "tool", "tool_call_id": "call_probe_1", "content": "391"},
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "calculator",
                "description": "Evaluate an arithmetic expression.",
                "parameters": {"type": "object", "properties": {"expression": {"type": "string"}},
                               "required": ["expression"]},
            },
        }],
    }


def send(item, model, with_reasoning):
    base = item["base_url"].rstrip("/")
    headers = {"Content-Type": "application/json", "Accept": "text/event-stream",
               "Authorization": f"Bearer {item.get('api_key') or ''}"}
    if base.startswith(ZEN_ROOT):
        headers.update({"User-Agent": ZEN_UA, "x-opencode-client": "cli", "x-opencode-project": "global",
                        "x-opencode-session": f"ses_{suffix()}", "x-opencode-request": f"msg_{suffix()}"})
    request = urllib.request.Request(f"{base}/chat/completions", method="POST", headers=headers,
                                     data=json.dumps(body(model, with_reasoning)).encode())
    result = {"status": None, "error": None, "text": "", "prompt_tokens": None}
    try:
        with urllib.request.urlopen(request, timeout=90) as response:
            result["status"] = response.status
            for raw in response:
                line = raw.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if payload == "[DONE]":
                    break
                try:
                    chunk = json.loads(payload)
                except json.JSONDecodeError:
                    continue
                if chunk.get("error"):
                    result["error"] = json.dumps(chunk["error"], ensure_ascii=False)[:300]
                for choice in chunk.get("choices") or []:
                    result["text"] += (choice.get("delta") or {}).get("content") or ""
                usage = chunk.get("usage") or {}
                if usage.get("prompt_tokens") is not None:
                    result["prompt_tokens"] = usage["prompt_tokens"]
    except urllib.error.HTTPError as error:
        result["status"] = error.code
        result["error"] = error.read().decode("utf-8", "replace")[:300]
    except Exception as error:  # 连接层失败也照实报,不抛
        result["error"] = f"{type(error).__name__}: {error}"[:300]
    return result


def main():
    args = sys.argv[1:]
    if not args or len(args) % 2:
        raise SystemExit(__doc__)
    config = json.loads(strip_jsonc(CONFIG.read_text("utf-8")))
    for provider_id, model in zip(args[::2], args[1::2]):
        item = provider(config, provider_id)
        print(f"== {provider_id} / {model}")
        runs = {}
        for label, with_reasoning in (("A 不带", False), ("B 带上", True)):
            runs[label] = run = send(item, model, with_reasoning)
            print(f"  {label}: HTTP {run['status']}  prompt_tokens={run['prompt_tokens']}  "
                  f"回复={run['text'].strip()[:60]!r}" + (f"  error={run['error']}" if run["error"] else ""))
        a, b = runs["A 不带"], runs["B 带上"]
        if b["status"] != 200 or b["error"]:
            verdict = "不收(带上就报错)"
        elif a["prompt_tokens"] and b["prompt_tokens"] and b["prompt_tokens"] > a["prompt_tokens"]:
            verdict = f"收下并转给了模型(prompt 多 {b['prompt_tokens'] - a['prompt_tokens']} token)"
        elif a["prompt_tokens"] and b["prompt_tokens"]:
            verdict = "收下但没转给模型(prompt 一样多)"
        else:
            verdict = "收下(没报用量,分不清转没转)"
        print(f"  结论:{verdict}")


if __name__ == "__main__":
    main()

---
name: script-creator
display_name: 脚本创作
summary: 写一个命令行脚本并注册成 Miyu 的工具
description: Write and register a Miyu script tool. Use when the user wants a new command-line script Miyu can call as a tool, or when a script fails to register or run — 写脚本、加个工具、注册脚本、脚本跑不起来。
compatibility: Miyu built-in script authoring workflow
---

# Script Creator

A script tool is one executable file. Miyu reads the tool contract from comment lines at the top of the file, runs the file with JSON arguments, and hands stdout back to the model.

## Calling manage_script

`manage_script` is not on the always-loaded tool list: registering a script is a
once-in-a-while action, so it lives behind this skill. Call it through the tool
bridge with `run_command`:

```bash
miyu tool-call manage_script --stdin <<'JSON'
{"action": "register", "path": "/abs/path/to/my-script"}
JSON
```

`miyu tool-call manage_script --describe` prints the full parameter contract.
Everything below that says "call `manage_script`" means this.

## Workflow

1. Write the script anywhere, the session workspace is fine. Start from a skeleton below.
2. Call `manage_script` with `action=register` and the absolute `path`. The file is copied into the scripts directory, made executable, and becomes a tool from the next tool round. Pass `scope=global` only when every persona should see it.
3. Call the new tool once with real arguments and read `success`, `exit_code`, `stdout` and `stderr` in the result.
4. To fix the script, edit the copy at the `path` that register returned and call the tool again. Code changes need no re-register. Header changes are picked up automatically as well.
5. `manage_script` with `action=list` shows registered, unregistered and disabled scripts with their directories.

## Runtime contract

- The first line must be a shebang such as `#!/usr/bin/env python3` or `#!/bin/bash`. Miyu executes the file directly.
- Arguments arrive as one JSON object on stdin. The same JSON is also in the environment variable `MIYU_ARGS_JSON` when it is under 64 KB.
- Nothing is passed on argv unless the header says `# Argv: flags`. That mode additionally expands `{"query":"x","limit":5,"json":true,"dry":false}` to `--query=x --limit=5 --json`. Keys are sorted, false and null are omitted, arrays and objects become JSON strings.
- Without a `Parameters` header the tool accepts any JSON object. The special key `stdin` then replaces the JSON on stdin with raw text.
- Print the result to stdout and exit 0. On failure exit non-zero and print a JSON object with `ok:false`, `error` and, when the user must act, `fix`. Miyu reports the exit code and the model reads your JSON.
- Default timeout is 120 seconds, maximum 300. Output beyond 20000 characters is cut before the model sees it, so cap lists and offer a `limit` parameter.
- `MIYU_SCRIPT_CACHE_DIR` points at Miyu's cache directory. Keep login profiles, cookies and caches under it. Fall back to XDG defaults when the variable is missing so the script also works from a terminal.
- Host queries (only when the header declares `Capabilities:`): Miyu injects `MIYU_HOST_TOKEN`, `MIYU_HOST_CAPABILITIES` and `MIYU_HOST_BIN`. Run `$MIYU_HOST_BIN host <method> [json]` — methods `host.info`, `providers.list`, `providers.get`, `subsystems.enabled`; stdout is one JSON line `{"ok":true,"data":…}` or `{"ok":false,"error":{"code","message"}}`. Provider data never includes API keys or endpoints. Treat a missing variable as "host unavailable" and keep working.
- Default to compact human-readable output and offer `format=json` for field-by-field processing.

## Header

Comment lines right after the shebang, written as `# Key: value`. Unknown comment lines such as a coding declaration are skipped. The header ends at the first code line.

```
#!/usr/bin/env python3
# 显示名称：番组日历
# Description: Query the Bangumi airing calendar. Use for "what airs today" and subject details.
# Timeout: 60
# Group: research
# Parameters:
# {
#   "type": "object",
#   "properties": {
#     "action": {"type": "string", "enum": ["calendar", "search"], "description": "calendar (default) or search"},
#     "query": {"type": "string", "description": "search keyword"}
#   }
# }
```

- `Description` is what the model sees. Write it in English. Keep the first sentence under 60 characters: in stub loading mode only that sentence is visible until the tool is loaded. Say what the tool does and when to use it first, caveats later.
- `显示名称` is the human-facing name and is required. Write it in Chinese. Without it a Chinese UI can only show the tool id.
- `Display name` is the English-UI name and is optional: the tool id is already English, so an English UI falls back to a humanized id (`xhs_search` becomes `Xhs search`). The two are separate slots, not aliases, so never put a Chinese name in `Display name` — that is what makes Chinese leak into an English UI.
- `Parameters` is a JSON Schema object with `type: object`. Give every property a `description`. Omit the block for free-form tools.
- `Id` overrides the tool name derived from the file name. `battery-care.py` becomes `battery_care` without it.
- `Group` places the tool in a `load_tools` group. `Argv: flags` turns on argv expansion. `Timeout` is in seconds.
- `index.json` in the scripts directory can override any header field per `id`. `manage_script` writes there only the fields you pass explicitly.

## Skeletons

Python, stdin JSON:

```python
#!/usr/bin/env python3
# 显示名称：示例
# Description: One sentence under 60 characters. Then when to use it.
# Parameters: {"type":"object","properties":{"query":{"type":"string","description":"what to look up"}},"required":["query"]}
import json
import os
import sys


def fail(error, fix=None, code=2):
    print(json.dumps({"ok": False, "error": error, "fix": fix}, ensure_ascii=False))
    sys.exit(code)


def main():
    raw = os.environ.get("MIYU_ARGS_JSON") or sys.stdin.read() or "{}"
    args = json.loads(raw)
    query = args.get("query")
    if not query:
        fail("query is required")
    print(f"result for {query}")


if __name__ == "__main__":
    main()
```

Bash, argv flags:

```bash
#!/usr/bin/env bash
# 显示名称：示例
# Description: One sentence under 60 characters.
# Argv: flags
# Parameters: {"type":"object","properties":{"query":{"type":"string","description":"what to look up"}},"required":["query"]}
set -euo pipefail
query=""
for arg in "$@"; do
  case "$arg" in
    --query=*) query="${arg#*=}" ;;
  esac
done
[ -n "$query" ] || { echo '{"ok":false,"error":"query is required"}'; exit 2; }
echo "result for $query"
```

## Rules

- Never ask the user to copy files into `~/.miyu`. Register does that.
- Never put secrets in the header or the description.
- Do not report the tool as working until you have called it once.
- Keep the script self-contained. Name third-party dependencies in a comment and fail with a `fix` message when they are missing.

#!/usr/bin/env python3
"""走查用的有状态 MCP 服务器：`count` 在进程里计数，结果带上自己的 pid；握手时给一段使用说明。
每起一次往 $MARKER 记一行 pid（列举工具那一次也算）。"""

import json
import os
import sys

INSTRUCTIONS = "Counter server: call count to advance the shared counter."

with open(os.environ["MARKER"], "a", encoding="utf-8") as marker:
    marker.write(f"{os.getpid()}\n")

count = 0
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request or "method" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "serverInfo": {"name": "counter", "version": "1"},
            "instructions": INSTRUCTIONS,
        }
    elif method == "tools/list":
        result = {"tools": [{
            "name": "count",
            "description": "Advance the counter and return its value.",
            "inputSchema": {"type": "object"},
        }]}
    elif method == "tools/call":
        count += 1
        result = {"content": [{"type": "text", "text": f"count {count} pid {os.getpid()}"}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)

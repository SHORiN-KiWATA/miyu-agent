#!/usr/bin/env python3
"""x-opencode-request A/B: does keeping the request header constant within a turn
change how much of the previous prompt OpenCode Go serves from cache?

Arm A = what Miyu sends today: a new x-opencode-request per HTTP request.
Arm B = what the opencode CLI sends: one id per user turn.

Every trial opens a fresh x-opencode-session and a prefix nobody has sent before,
then runs one simulated turn: STEPS requests, each appending a small
assistant/tool-sized pair. Step k >= 2 is scored by how much of step k-1's
prompt came back as cached. Arms alternate trial by trial so a gateway incident
hits both.

Reads the provider key from ~/.miyu/config/config.jsonc and never prints it.
Results: one JSON line per request under ~/.cache/miyu-cache-ab/.
"""

import argparse
import hashlib
import json
import os
import random
import re
import secrets
import statistics
import sys
import time
from pathlib import Path

import requests

USER_AGENT = "opencode/1.18.29 ai-sdk/provider-utils/4.0.46 runtime/bun/1.4.0"
CONFIG = Path.home() / ".miyu/config/config.jsonc"
OUT_DIR = Path.home() / ".cache/miyu-cache-ab"
WORDS = (
    "river stone lantern orbit maple cipher harbor quartz meadow signal ember "
    "falcon prism tundra velvet anchor comet garnet willow beacon canyon drift "
    "summit thistle marble nebula copper glacier saffron tidal"
).split()


def load_provider(provider_id):
    text = CONFIG.read_text(encoding="utf-8")
    text = re.sub(r"(?m)^\s*//.*$", "", text)
    text = re.sub(r",(\s*[}\]])", r"\1", text)
    for provider in json.loads(text).get("providers", []):
        if provider.get("id") == provider_id:
            return provider["base_url"].rstrip("/"), provider["api_key"]
    sys.exit(f"provider {provider_id} not found")


def opencode_id(prefix, value):
    return f"{prefix}_{hashlib.sha1(value.encode()).hexdigest()[:26]}"


def filler(rng, words):
    return " ".join(rng.choice(WORDS) for _ in range(words))


def base_messages(nonce, prefix_words):
    rng = random.Random(nonce)
    system = f"Session {nonce}. You are a terse assistant. Reply with one short sentence."
    history = []
    for index in range(8):
        history.append({"role": "user", "content": f"Note {index}: " + filler(rng, prefix_words // 8)})
        history.append({"role": "assistant", "content": f"Noted {index}."})
    return [{"role": "system", "content": system}] + history + [
        {"role": "user", "content": "Summarize the notes in five words."}
    ]


def step_pair(nonce, step):
    rng = random.Random(f"{nonce}-{step}")
    return [
        {"role": "assistant", "content": f"Checking part {step}."},
        {"role": "user", "content": f"Result {step}: " + filler(rng, 300)},
    ]


def cached_tokens(usage):
    for key in ("prompt_cache_hit_tokens", "cache_read_input_tokens"):
        if isinstance(usage.get(key), int):
            return usage[key]
    details = usage.get("prompt_tokens_details") or {}
    return details.get("cached_tokens") or 0


def send(base_url, key, model, messages, session, request_id, max_tokens):
    headers = {
        "Authorization": f"Bearer {key}",
        "Content-Type": "application/json",
        "User-Agent": USER_AGENT,
        "x-opencode-client": "cli",
        "x-opencode-project": "global",
        "x-opencode-session": session,
        "x-opencode-request": request_id,
    }
    body = {
        "model": model,
        "messages": messages,
        "stream": True,
        "stream_options": {"include_usage": True},
        "temperature": 0.6,
        "max_tokens": max_tokens,
    }
    started = time.time()
    response = requests.post(f"{base_url}/chat/completions", headers=headers, json=body, stream=True, timeout=180)
    if response.status_code != 200:
        return {"status": response.status_code, "error": response.text[:300], "latency": time.time() - started}
    usage, response_id, fingerprint, served_model = None, None, None, None
    for raw in response.iter_lines(decode_unicode=True):
        if not raw or not raw.startswith("data:"):
            continue
        data = raw[5:].strip()
        if data == "[DONE]":
            break
        try:
            chunk = json.loads(data)
        except ValueError:
            continue
        response_id = response_id or chunk.get("id")
        fingerprint = fingerprint or chunk.get("system_fingerprint")
        served_model = served_model or chunk.get("model")
        if chunk.get("usage"):
            usage = chunk["usage"]
    return {
        "status": 200,
        "latency": round(time.time() - started, 2),
        "usage": usage,
        "id": response_id,
        "fingerprint": fingerprint,
        "model": served_model,
    }


def run_trial(args, base_url, key, arm, trial, sink):
    nonce = secrets.token_hex(8)
    session = opencode_id("ses", f"ab-{nonce}")
    turn_request = opencode_id("msg", f"turn-{nonce}")
    messages = base_messages(nonce, args.prefix_words)
    previous_prompt = None
    rows = []
    for step in range(1, args.steps + 1):
        request_id = turn_request if arm == "B" else opencode_id("msg", f"{nonce}-{step}")
        result = send(base_url, key, args.model, messages, session, request_id, args.max_tokens)
        usage = result.get("usage") or {}
        prompt = usage.get("prompt_tokens") or 0
        cached = cached_tokens(usage) if usage else 0
        row = {
            "ts": time.strftime("%H:%M:%S"),
            "arm": arm,
            "trial": trial,
            "step": step,
            "status": result.get("status"),
            "prompt": prompt,
            "cached": cached,
            "prev_prompt": previous_prompt,
            "hit_vs_prev": round(cached / previous_prompt, 4) if previous_prompt else None,
            "latency": result.get("latency"),
            "id_shape": re.sub(r"[0-9a-f]", "h", (result.get("id") or "")[:12]),
            "fingerprint": result.get("fingerprint"),
            "model": result.get("model"),
            "usage_keys": sorted(usage.keys()),
            "error": result.get("error"),
        }
        sink.write(json.dumps(row) + "\n")
        sink.flush()
        rows.append(row)
        print(f"{row['ts']} {arm} t{trial} s{step} status={row['status']} prompt={prompt} cached={cached} "
              f"hit_vs_prev={row['hit_vs_prev']} {row['id_shape']} {row['fingerprint']}", flush=True)
        if result.get("status") != 200:
            time.sleep(args.backoff)
            return rows
        previous_prompt = prompt
        messages = messages + step_pair(nonce, step)
        time.sleep(args.gap)
    return rows


def summarize(rows):
    for arm in ("A", "B"):
        scored = [r["hit_vs_prev"] for r in rows if r["arm"] == arm and r["hit_vs_prev"] is not None]
        if not scored:
            continue
        missing = [r["prev_prompt"] - r["cached"] for r in rows
                   if r["arm"] == arm and r["hit_vs_prev"] is not None]
        # 上游按 64 token 一块存,上一请求末尾不满一块的那点天然不命中;
        # 缺超过两块才算真丢。
        drops = [m for m in missing if m > 128]
        print(f"arm {arm}: scored={len(scored)} mean_hit_vs_prev={statistics.mean(scored):.4f} "
              f"min={min(scored):.4f} drops(>128 missing)={len(drops)} lost={sum(drops)}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--provider", default="opencodego")
    parser.add_argument("--model", default="deepseek-v4.1-flash")
    parser.add_argument("--trials", type=int, default=12, help="per arm")
    parser.add_argument("--steps", type=int, default=6)
    parser.add_argument("--prefix-words", type=int, default=9000)
    parser.add_argument("--max-tokens", type=int, default=64)
    parser.add_argument("--gap", type=float, default=1.0, help="seconds between steps")
    parser.add_argument("--backoff", type=float, default=30.0)
    parser.add_argument("--summarize", help="only summarize an existing result file")
    args = parser.parse_args()
    if args.summarize:
        summarize([json.loads(line) for line in open(args.summarize, encoding="utf-8")])
        return
    base_url, key = load_provider(args.provider)
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    out = OUT_DIR / f"zen-request-ab-{time.strftime('%m%d-%H%M%S')}.jsonl"
    print(f"results -> {out}", flush=True)
    rows = []
    with open(out, "w", encoding="utf-8") as sink:
        for trial in range(args.trials):
            for arm in (("A", "B") if trial % 2 == 0 else ("B", "A")):
                rows += run_trial(args, base_url, key, arm, trial, sink)
    summarize(rows)


if __name__ == "__main__":
    main()

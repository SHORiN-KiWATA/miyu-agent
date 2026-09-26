#!/usr/bin/env python3
"""OpenCode Go(deepseek-v4.1-flash)供应商侧缓存直连探针(09-25)。不经过 Miyu,直接打 chat/completions,
头照 crates/miyu-core/src/llm/openai_compatible/zen_headers.rs 的形状。每个试次一个新 nonce、新会话头,
前缀谁都没发过,试次之间不串缓存。

子命令(每项默认重复 3 次,看倍率别看单次):
    header      x-opencode-request 每请求换(A,Miyu 现状)vs 一轮一个(B,opencode CLI);一轮 6 步,交替跑
    readiness   请求 1 结束后隔 0/1/3/10 秒发请求 2(追加一小段),请求 1 的 prompt 盖住了多少
    concurrency 同一前缀 4 个请求同时发(同一会话头)→ 再各追加一段同时发;另测 4 个不同会话头能否吃到 s0 暖好的前缀
    ttl         请求 1 之后空闲 60/180/300/600 秒再发请求 2,缓存还在不在(各试次并行跑)
    reasoning   带工具调用的上一轮:思考原样回传(Miyu 现状)/ 下一轮去掉 / 从不回传,下一轮首请求在哪断
    granularity 汇总所有结果里 cached 对 64/128/256 取模

    go_cache_probe.py <子命令> [--reps 3] [--prefix-words 16000]
    go_cache_probe.py all          依次跑除 ttl 以外的全部
key 从 ~/.miyu/config/config.jsonc 只读取、不打印。每条请求一行 JSON 写进 ~/.cache/miyu-cache-live/probe/。
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
import threading
import time
from pathlib import Path

import requests

USER_AGENT = "opencode/1.18.29 ai-sdk/provider-utils/4.0.46 runtime/bun/1.4.0"
CONFIG = Path.home() / ".miyu/config/config.jsonc"
OUT = Path.home() / ".cache/miyu-cache-live/probe"
PROVIDER = "opencodego"
MODEL = "deepseek-v4.1-flash"
GRANULE = 128
WORDS = (
    "river stone lantern orbit maple cipher harbor quartz meadow signal ember falcon prism tundra velvet anchor "
    "comet garnet willow beacon canyon drift summit thistle marble nebula copper glacier saffron tidal"
).split()
TOOLS = [{
    "type": "function",
    "function": {
        "name": "run_command",
        "description": "Run a shell command and return its output.",
        "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]},
    },
}]
LOCK = threading.Lock()


def load_provider():
    text = CONFIG.read_text(encoding="utf-8")
    text = re.sub(r"(?m)^\s*//.*$", "", text)
    text = re.sub(r",(\s*[}\]])", r"\1", text)
    for provider in json.loads(text).get("providers", []):
        if provider.get("id") == PROVIDER:
            return provider["base_url"].rstrip("/"), provider["api_key"]
    sys.exit(f"provider {PROVIDER} not found")


BASE_URL, KEY = load_provider()


def oc_id(prefix, value):
    return f"{prefix}_{hashlib.sha1(value.encode()).hexdigest()[:26]}"


def filler(seed, words):
    rng = random.Random(seed)
    return " ".join(rng.choice(WORDS) for _ in range(words))


def prefix_messages(nonce, words):
    system = f"Session {nonce}. You are a terse assistant. Reply with one short sentence."
    history = []
    for index in range(8):
        history.append({"role": "user", "content": f"Note {index}: " + filler(f"{nonce}-{index}", words // 8)})
        history.append({"role": "assistant", "content": f"Noted {index}."})
    return [{"role": "system", "content": system}] + history


def cached_of(usage):
    for key in ("prompt_cache_hit_tokens", "cache_read_input_tokens"):
        if isinstance(usage.get(key), int):
            return usage[key]
    return (usage.get("prompt_tokens_details") or {}).get("cached_tokens") or 0


def send(messages, session, request, tools=None, max_tokens=24):
    headers = {
        "Authorization": f"Bearer {KEY}",
        "Content-Type": "application/json",
        "User-Agent": USER_AGENT,
        "x-opencode-client": "cli",
        "x-opencode-project": "global",
        "x-opencode-session": session,
        "x-opencode-request": request,
    }
    body = {"model": MODEL, "messages": messages, "stream": True, "stream_options": {"include_usage": True},
            "temperature": 0.6, "max_tokens": max_tokens}
    if tools:
        body["tools"] = tools
    started = time.time()
    for attempt in range(4):
        try:
            response = requests.post(f"{BASE_URL}/chat/completions", headers=headers, json=body, stream=True,
                                     timeout=240)
        except requests.RequestException as error:
            if attempt == 3:
                return {"status": 0, "error": str(error)[:200]}
            time.sleep(10 * (attempt + 1))
            continue
        if response.status_code in (429, 500, 502, 503, 504) and attempt < 3:
            time.sleep(15 * (attempt + 1))
            continue
        if response.status_code != 200:
            return {"status": response.status_code, "error": response.text[:300]}
        usage, served = None, None
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
            served = served or chunk.get("model")
            if chunk.get("usage"):
                usage = chunk["usage"]
        usage = usage or {}
        return {"status": 200, "prompt": usage.get("prompt_tokens") or 0, "cached": cached_of(usage),
                "completion": usage.get("completion_tokens") or 0, "served": served,
                "done_at": time.time(), "latency": round(time.time() - started, 2)}
    return {"status": 0, "error": "retries exhausted"}


class Sink:
    def __init__(self, name):
        OUT.mkdir(parents=True, exist_ok=True)
        # 带上进程号:两个探针同一秒起跑时各写各的文件(09-25 两组 ttl 撞过一次)。
        self.path = OUT / f"{name}-{time.strftime('%m%d-%H%M%S')}-{os.getpid()}.jsonl"
        self.handle = open(self.path, "w", encoding="utf-8")
        self.rows = []

    def write(self, **row):
        row["ts"] = time.strftime("%H:%M:%S")
        with LOCK:
            self.rows.append(row)
            self.handle.write(json.dumps(row, ensure_ascii=False) + "\n")
            self.handle.flush()
            print(json.dumps(row, ensure_ascii=False), flush=True)


def coverage(cached, previous_prompt):
    return round(min(cached, previous_prompt) / previous_prompt, 4) if previous_prompt else None


# ------------------------------------------------------------------ (a) header

def run_header(args):
    sink = Sink("header")
    for trial in range(args.reps):
        for arm in (("A", "B") if trial % 2 == 0 else ("B", "A")):
            nonce = secrets.token_hex(8)
            session = oc_id("ses", f"hdr-{nonce}")
            turn_request = oc_id("msg", f"turn-{nonce}")
            messages = prefix_messages(nonce, args.prefix_words) + [
                {"role": "user", "content": "Summarize the notes in five words."}]
            previous = None
            for step in range(1, 7):
                request = turn_request if arm == "B" else oc_id("msg", f"{nonce}-{step}")
                result = send(messages, session, request)
                sink.write(test="header", arm=arm, trial=trial, step=step, prev_prompt=previous,
                           coverage=coverage(result.get("cached", 0), previous) if previous else None, **result)
                if result.get("status") != 200:
                    break
                previous = result["prompt"]
                messages = messages + [
                    {"role": "assistant", "content": f"Checking part {step}."},
                    {"role": "user", "content": f"Result {step}: " + filler(f"{nonce}-s{step}", 500)}]
                time.sleep(1.0)
    summarize_header(sink.rows)


def summarize_header(rows):
    for arm in ("A", "B"):
        scored = [r for r in rows if r.get("arm") == arm and r.get("coverage") is not None]
        if scored:
            missing = [r["prev_prompt"] - min(r["cached"], r["prev_prompt"]) for r in scored]
            print(f"header arm {arm}: n={len(scored)} mean_coverage={statistics.mean(r['coverage'] for r in scored):.4f} "
                  f"min={min(r['coverage'] for r in scored):.4f} missing>={GRANULE}: "
                  f"{sum(1 for m in missing if m >= GRANULE)}")


# ------------------------------------------------------------------ (b) readiness

def run_readiness(args):
    sink = Sink("readiness")
    for rep in range(args.reps):
        for gap in (0, 1, 3, 10):
            nonce = secrets.token_hex(8)
            session = oc_id("ses", f"rdy-{nonce}")
            messages = prefix_messages(nonce, args.prefix_words) + [
                {"role": "user", "content": "Summarize the notes in five words."}]
            first = send(messages, session, oc_id("msg", f"{nonce}-1"))
            if first.get("status") != 200:
                sink.write(test="readiness", gap=gap, rep=rep, stage=1, **first)
                continue
            time.sleep(gap)
            messages = messages + [{"role": "assistant", "content": "Checking."},
                                   {"role": "user", "content": "More: " + filler(f"{nonce}-more", 500)}]
            second = send(messages, session, oc_id("msg", f"{nonce}-2"))
            sink.write(test="readiness", gap=gap, rep=rep, first_prompt=first["prompt"], first_cached=first["cached"],
                       coverage=coverage(second.get("cached", 0), first["prompt"]), **second)
    for gap in (0, 1, 3, 10):
        values = [r["coverage"] for r in sink.rows if r.get("gap") == gap and r.get("coverage") is not None]
        if values:
            print(f"readiness gap={gap:>2}s: n={len(values)} coverage mean={statistics.mean(values):.4f} "
                  f"min={min(values):.4f}")


# ------------------------------------------------------------------ (c) concurrency

def parallel(jobs):
    results = [None] * len(jobs)

    def worker(index, job):
        results[index] = job()

    threads = [threading.Thread(target=worker, args=(i, job)) for i, job in enumerate(jobs)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    return results


def run_concurrency(args):
    sink = Sink("concurrency")
    for rep in range(args.reps):
        # c1:同一会话头,冷前缀 4 路同时发 → 各自追加再 4 路同时发
        nonce = secrets.token_hex(8)
        session = oc_id("ses", f"cc-{nonce}")
        base = prefix_messages(nonce, args.prefix_words)
        waves = [base + [{"role": "user", "content": f"Question {i}: summarize in {i + 3} words."}] for i in range(4)]
        cold = parallel([lambda m=m, i=i: send(m, session, oc_id("msg", f"{nonce}-c{i}")) for i, m in enumerate(waves)])
        for i, result in enumerate(cold):
            sink.write(test="concurrency", case="same_session_cold", rep=rep, lane=i, **result)
        time.sleep(2)
        seconds = [m + [{"role": "assistant", "content": "Done."},
                        {"role": "user", "content": "Next: " + filler(f"{nonce}-n{i}", 300)}] for i, m in enumerate(waves)]
        warm = parallel([lambda m=m, i=i: send(m, session, oc_id("msg", f"{nonce}-w{i}")) for i, m in enumerate(seconds)])
        for i, result in enumerate(warm):
            previous = cold[i].get("prompt") or 0
            sink.write(test="concurrency", case="same_session_second_wave", rep=rep, lane=i, prev_prompt=previous,
                       coverage=coverage(result.get("cached", 0), previous), **result)
        # c2:s0 先把前缀暖好,再换 4 个不同会话头(像 4 个子代理)各发一次
        nonce = secrets.token_hex(8)
        base = prefix_messages(nonce, args.prefix_words)
        warm0 = send(base + [{"role": "user", "content": "Warm up: say ok."}], oc_id("ses", f"x0-{nonce}"),
                     oc_id("msg", f"{nonce}-x0"))
        sink.write(test="concurrency", case="cross_session_warm", rep=rep, lane=0, **warm0)
        time.sleep(2)
        others = parallel([lambda i=i: send(base + [{"role": "user", "content": f"Lane {i}: say ok."}],
                                            oc_id("ses", f"x{i}-{nonce}"), oc_id("msg", f"{nonce}-x{i}"))
                           for i in range(1, 5)])
        for i, result in enumerate(others, 1):
            shared = warm0.get("prompt") or 0
            sink.write(test="concurrency", case="cross_session_after_warm", rep=rep, lane=i, shared_prompt=shared,
                       coverage=coverage(result.get("cached", 0), shared), **result)
    for case in ("same_session_cold", "same_session_second_wave", "cross_session_after_warm"):
        rows = [r for r in sink.rows if r.get("case") == case and r.get("status") == 200]
        if rows:
            cached = [r["cached"] for r in rows]
            cov = [r["coverage"] for r in rows if r.get("coverage") is not None]
            print(f"concurrency {case}: n={len(rows)} cached mean={statistics.mean(cached):.0f} "
                  f"zero={sum(1 for c in cached if c == 0)}"
                  + (f" coverage mean={statistics.mean(cov):.4f} min={min(cov):.4f}" if cov else ""))


# ------------------------------------------------------------------ (e) ttl

def run_ttl(args):
    sink = Sink("ttl")
    idles = [int(x) for x in args.idles.split(",")]

    def trial(idle, rep):
        nonce = secrets.token_hex(8)
        session = oc_id("ses", f"ttl-{nonce}")
        messages = prefix_messages(nonce, args.prefix_words) + [
            {"role": "user", "content": "Summarize the notes in five words."}]
        first = send(messages, session, oc_id("msg", f"{nonce}-1"))
        if first.get("status") != 200:
            sink.write(test="ttl", idle=idle, rep=rep, stage=1, **first)
            return
        time.sleep(idle)
        messages = messages + [{"role": "assistant", "content": "Done."},
                               {"role": "user", "content": "Next: say ok."}]
        second = send(messages, session, oc_id("msg", f"{nonce}-2"))
        sink.write(test="ttl", idle=idle, rep=rep, first_prompt=first["prompt"],
                   coverage=coverage(second.get("cached", 0), first["prompt"]), **second)

    jobs = []
    for rep in range(args.reps):
        for idle in idles:
            jobs.append(lambda idle=idle, rep=rep: trial(idle, rep))
    # 错开 3 秒起跑,别让一堆冷请求同时砸上去
    threads = []
    for job in jobs:
        thread = threading.Thread(target=job)
        thread.start()
        threads.append(thread)
        time.sleep(3)
    for thread in threads:
        thread.join()
    for idle in idles:
        values = [r["coverage"] for r in sink.rows if r.get("idle") == idle and r.get("coverage") is not None]
        if values:
            print(f"ttl idle={idle:>4}s: n={len(values)} coverage={['%.3f' % v for v in values]}")


# ------------------------------------------------------------------ (f) reasoning

REASONING = ("Let me think about what the user wants. They asked how many lines the file has, so I should run wc -l "
             "on it and then report the number back. ") * 12


def run_reasoning(args):
    sink = Sink("reasoning")
    variants = ("keep", "strip_next_turn", "never")
    for rep in range(args.reps):
        for variant in variants:
            nonce = secrets.token_hex(8)
            session = oc_id("ses", f"rsn-{nonce}")
            doc = filler(f"{nonce}-doc", args.prefix_words)
            base = [{"role": "system", "content": f"Session {nonce}. You are a helpful assistant with tools."},
                    {"role": "user", "content": "Here is a document:\n" + doc},
                    {"role": "assistant", "content": "Got it."},
                    {"role": "user", "content": "How many lines does data.txt have?"},
                    {"role": "user", "content": "<runtime now=\"2026-09-25 Fri 21:00\" cwd=\"/tmp/w\"/>"}]
            r1 = send(base, session, oc_id("msg", f"{nonce}-r1"), tools=TOOLS)
            call = {"id": f"call_{nonce[:8]}", "type": "function",
                    "function": {"name": "run_command", "arguments": json.dumps({"command": "wc -l data.txt"})}}
            within_reasoning = "" if variant == "never" else REASONING
            tool_turn = [{"role": "assistant", "content": "", "tool_calls": [call],
                          "reasoning_content": within_reasoning},
                         {"role": "tool", "tool_call_id": call["id"],
                          "content": "1234 data.txt\n" + filler(f"{nonce}-out", 900)}]
            time.sleep(1)
            r2 = send(base + tool_turn, session, oc_id("msg", f"{nonce}-r2"), tools=TOOLS)
            next_turn_reasoning = REASONING if variant == "keep" else ""
            history = base + [dict(tool_turn[0], reasoning_content=next_turn_reasoning), tool_turn[1],
                              {"role": "assistant", "content": "data.txt has 1234 lines."},
                              {"role": "user", "content": "Thanks. And what is 2+2?"},
                              {"role": "user", "content": "<runtime now=\"2026-09-25 Fri 21:01\" cwd=\"/tmp/w\"/>"}]
            time.sleep(3)
            r3 = send(history, session, oc_id("msg", f"{nonce}-r3"), tools=TOOLS)
            sink.write(test="reasoning", variant=variant, rep=rep,
                       r1_prompt=r1.get("prompt"), r1_cached=r1.get("cached"),
                       r2_prompt=r2.get("prompt"), r2_cached=r2.get("cached"),
                       r2_coverage=coverage(r2.get("cached", 0), r1.get("prompt") or 0),
                       r3_prompt=r3.get("prompt"), r3_cached=r3.get("cached"),
                       r3_coverage=coverage(r3.get("cached", 0), r2.get("prompt") or 0),
                       r3_minus_r2=(r3.get("prompt") or 0) - (r2.get("prompt") or 0),
                       status=[r1.get("status"), r2.get("status"), r3.get("status")])
    for variant in variants:
        rows = [r for r in sink.rows if r.get("variant") == variant]
        if rows:
            print(f"reasoning {variant:<16}: within cov={[r['r2_coverage'] for r in rows]} "
                  f"cross cov={[r['r3_coverage'] for r in rows]} r3-r2 prompt={[r['r3_minus_r2'] for r in rows]} "
                  f"r3 cached={[r['r3_cached'] for r in rows]} r1 prompt={[r['r1_prompt'] for r in rows]}")


# ------------------------------------------------------------------ (d) granularity

def run_granularity(_args):
    cached = []
    for path in OUT.glob("*.jsonl"):
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            for key in ("cached", "r1_cached", "r2_cached", "r3_cached"):
                if row.get(key):
                    cached.append(row[key])
    if not cached:
        print("no data")
        return
    for unit in (64, 128, 256, 512):
        print(f"granularity: {sum(1 for c in cached if c % unit == 0)}/{len(cached)} cached values are multiples of {unit}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("header", "readiness", "concurrency", "ttl", "reasoning", "granularity",
                                            "all"))
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--prefix-words", type=int, default=16000)
    parser.add_argument("--idles", default="60,180,300,600")
    args = parser.parse_args()
    table = {"header": run_header, "readiness": run_readiness, "concurrency": run_concurrency, "ttl": run_ttl,
             "reasoning": run_reasoning, "granularity": run_granularity}
    if args.command == "all":
        for name in ("reasoning", "readiness", "concurrency", "header", "granularity"):
            table[name](args)
    else:
        table[args.command](args)
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""低占用专项 A/B 测量:对指定二进制量 daemon/REPL 的内存与 CPU 足迹。

用法:
    BIN=/usr/bin/miyu python3 run.py baseline
    BIN=../../target/release/miyu python3 run.py round2

隔离手法(见 memory/miyu-live-testing-notes):独立 MIYU_HOME + 独立
XDG_RUNTIME_DIR + daemon 显式 `__daemon --port 18490`,与线上 8300 完全
不相交;provider 只有本地桩,无真实出网(models.dev 后台刷新除外,两组
同样发生,A/B 公平)。

量的东西:
  daemon: 落定 RSS(smaps anon/file)、30s 空闲窗 CPU% 与 ctx-switch 速率、
          5 个流式回合期间的 CPU 秒与每回合 ctx-switch、回合后 RSS。
  REPL:   附着态(连 daemon)空载 RSS。
结果写 results/<tag>.json。
"""
import fcntl
import json
import os
import pty
import re
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

BASE = Path(__file__).resolve().parent
REPO = BASE.parents[1]
BIN = Path(os.environ.get("BIN", REPO / "target" / "release" / "miyu")).resolve()
TAG = sys.argv[1] if len(sys.argv) > 1 else "run"
HOME = BASE / "homes" / TAG
RUN_DIR = BASE / "xdg-run" / TAG
RESULTS = BASE / "results"
WEB_PORT = int(os.environ.get("WEB_PORT", "18490"))
STUB_PORT = int(os.environ.get("STUB_PORT", "18491"))
TURNS = int(os.environ.get("TURNS", "5"))
IDLE_SECONDS = float(os.environ.get("IDLE_SECONDS", "30"))

sys.path.insert(0, str(REPO / "testkit" / "persona-ab"))
from run import strip_jsonc  # noqa: E402

REAL_CONFIG = Path.home() / ".miyu" / "config" / "config.jsonc"
REAL_MODELS_CACHE = Path.home() / ".miyu" / "cache" / "models_cache.json"


def build_home():
    if HOME.exists():
        shutil.rmtree(HOME)
    if RUN_DIR.exists():
        shutil.rmtree(RUN_DIR)
    (HOME / "config").mkdir(parents=True)
    RUN_DIR.mkdir(parents=True)
    cfg = json.loads(strip_jsonc(REAL_CONFIG.read_text()), strict=False)
    for key in ("platforms", "web", "voice", "alarm"):
        cfg.pop(key, None)
    cfg["providers"] = [{
        "enabled": True,
        "id": "stub",
        "display_name": "Stub",
        "base_url": f"http://127.0.0.1:{STUB_PORT}/v1",
        "protocol": "openai-chat",
        "api_key": "stub-key",
        "models": ["stub-model"],
    }]
    cfg["active_provider_models"] = [{"provider_id": "stub", "model": "stub-model"}]
    cfg.pop("active_multimodal_provider_models", None)
    cfg.setdefault("memory", {})["enabled"] = False
    cfg.setdefault("cache", {})["request_log"] = False
    (HOME / "config" / "config.jsonc").write_text(
        json.dumps(cfg, ensure_ascii=False, indent=2)
    )
    if REAL_MODELS_CACHE.exists():
        (HOME / "cache").mkdir(parents=True, exist_ok=True)
        shutil.copy(REAL_MODELS_CACHE, HOME / "cache" / "models_cache.json")


def env_for(extra=None):
    env = dict(os.environ)
    for k in ("XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "MIYU_DIRECT"):
        env.pop(k, None)
    env["MIYU_HOME"] = str(HOME)
    env["XDG_RUNTIME_DIR"] = str(RUN_DIR)
    env["TERM"] = "xterm-256color"
    if extra:
        env.update(extra)
    return env


def smaps(pid):
    text = Path(f"/proc/{pid}/smaps_rollup").read_text()
    def field(name):
        m = re.search(rf"^{name}:\s+(\d+) kB", text, re.M)
        return int(m.group(1)) if m else 0
    return {"rss_kb": field("Rss"), "anon_kb": field("Anonymous"),
            "file_clean_kb": field("Shared_Clean") + field("Private_Clean")}


def jiffies(pid):
    parts = Path(f"/proc/{pid}/stat").read_text().split()
    return int(parts[13]) + int(parts[14])


def ctx_switches(pid):
    total = 0
    for task in Path(f"/proc/{pid}/task").iterdir():
        try:
            text = (task / "status").read_text()
        except FileNotFoundError:
            continue
        for name in ("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches"):
            m = re.search(rf"^{name}:\s+(\d+)", text, re.M)
            if m:
                total += int(m.group(1))
    return total


def hwm(pid):
    m = re.search(r"^VmHWM:\s+(\d+) kB", Path(f"/proc/{pid}/status").read_text(), re.M)
    return int(m.group(1)) if m else 0


class Repl:
    """PTY 里附着 daemon 的 REPL,应答 ESC[6n。"""

    def __init__(self):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        self.proc = subprocess.Popen(
            [str(BIN)], stdin=slave, stdout=slave, stderr=slave,
            env=env_for(), close_fds=True, preexec_fn=os.setsid,
        )
        os.close(slave)

    def pump(self, seconds):
        import select
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([self.master], [], [], 0.2)
            if self.master in r:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    return
                if b"\x1b[6n" in chunk:
                    os.write(self.master, b"\x1b[24;1R")

    def send(self, text):
        os.write(self.master, text.encode() + b"\r")

    def close(self):
        try:
            self.send("/exit")
            self.pump(2)
        except OSError:
            pass
        try:
            os.kill(self.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.proc.wait()


def stub_log_entries(log_path):
    if not log_path.exists():
        return []
    return [json.loads(line) for line in log_path.read_text().splitlines() if line.strip()]


def main():
    assert BIN.exists(), f"binary not found: {BIN}"
    build_home()
    RESULTS.mkdir(exist_ok=True)
    stub_log = BASE / f"stub-{TAG}.jsonl"
    stub_log.unlink(missing_ok=True)

    stub = subprocess.Popen(
        [sys.executable, str(BASE / "stub_llm.py")],
        env={**os.environ, "STUB_PORT": str(STUB_PORT), "STUB_LOG": str(stub_log)},
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    daemon = subprocess.Popen(
        [str(BIN), "__daemon", "--port", str(WEB_PORT)],
        env=env_for(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        preexec_fn=os.setsid,
    )
    result = {"bin": str(BIN), "tag": TAG}
    try:
        time.sleep(8)
        assert daemon.poll() is None, "daemon died during startup"
        result["daemon_settle"] = smaps(daemon.pid)

        j0, c0, t0 = jiffies(daemon.pid), ctx_switches(daemon.pid), time.time()
        time.sleep(IDLE_SECONDS)
        dt = time.time() - t0
        result["daemon_idle"] = {
            "cpu_pct": (jiffies(daemon.pid) - j0) / os.sysconf("SC_CLK_TCK") / dt * 100,
            "ctx_switch_per_s": (ctx_switches(daemon.pid) - c0) / dt,
            "window_s": dt,
        }

        repl = Repl()
        repl.pump(6)
        result["repl_startup"] = smaps(repl.proc.pid)

        jt0, ct0, tt0 = jiffies(daemon.pid), ctx_switches(daemon.pid), time.time()
        for i in range(TURNS):
            repl.send(f"占用测量回合{i}")
            deadline = time.time() + 60
            target_ends = i + 1
            while time.time() < deadline:
                repl.pump(1)
                ends = [e for e in stub_log_entries(stub_log) if e["event"] == "end"]
                if len(ends) >= target_ends:
                    break
            repl.pump(2)
        ttd = time.time() - tt0
        result["turns"] = {
            "count": TURNS,
            "daemon_cpu_s": (jiffies(daemon.pid) - jt0) / os.sysconf("SC_CLK_TCK"),
            "daemon_ctx_switch_per_turn": (ctx_switches(daemon.pid) - ct0) / TURNS,
            "window_s": ttd,
        }
        result["repl_after_turns"] = smaps(repl.proc.pid)
        result["daemon_after_turns"] = smaps(daemon.pid)
        result["daemon_hwm_kb"] = hwm(daemon.pid)
        repl.close()
        time.sleep(2)
        result["daemon_after_repl_exit"] = smaps(daemon.pid)
    finally:
        try:
            os.killpg(daemon.pid, signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            daemon.terminate()
        stub.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
        stub.wait()

    out = RESULTS / f"{TAG}.json"
    out.write_text(json.dumps(result, indent=2, ensure_ascii=False))
    print(json.dumps(result, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()

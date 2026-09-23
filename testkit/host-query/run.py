#!/usr/bin/env python3
"""脚本查宿主信息的端到端实测(09-16 接口治理):隔离 home + 独立端口 daemon + 桩 LLM。

链路:头部声明 `Capabilities:` 的脚本 → daemon 拉起时注入一次性令牌 →
脚本跑 `$MIYU_HOST_BIN host providers.list / host.info / subsystems.enabled` →
IPC HostQuery → daemon 按令牌找授权集 → 脱敏摘要回到脚本 stdout → 桩 LLM 把工具结果
原样回成正文 → `miyu ask --output-format json` 的 done.text 里读回来断言。

断言:providers.list 拿到 id=stub 且没有 api key;host.info 报契约版本;
subsystems.enabled 没授权 → permission_denied;脚本退出即令牌作废(单测已覆盖)。

用法:先 `cargo build`,再 `python3 testkit/host-query/run.py`。绝不触碰线上 8300 daemon。
产物:testkit/host-query/out/(daemon.log、verdict.json)。
"""
import importlib.util
import json
import os
import shutil
import stat
import subprocess
import sys
import time
from pathlib import Path

# 跑测具的进程多半坐在某个 herdr pane 里(AI 会话的终端):它的 HERDR_* 漏给被测的 miyu,
# 被测进程就会往那个 pane 报状态、认领它,把人正在看的侧栏搅乱(09-23)。
for _herdr_key in [key for key in os.environ if key.startswith("HERDR_")]:
    del os.environ[_herdr_key]

REPO = Path(__file__).resolve().parents[2]
MIYU = Path(os.environ.get("BIN") or REPO / "target" / "debug" / "miyu")
BASE = Path(__file__).resolve().parent
OUT = BASE / "out"
HOME = BASE / "home"
RUN = Path.home() / ".cache" / "miyu-hostq-run"
PORT = 18397
STUB_PORT = 18496

spec = importlib.util.spec_from_file_location("persona_ab", REPO / "testkit" / "persona-ab" / "run.py")
persona_ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(persona_ab)

SCRIPT = '''#!/usr/bin/env python3
# Description: Probe the Miyu host query API and report what it returns.
# 显示名称：宿主探针
# Permission: read-only
# Capabilities: providers.read, host.info
import json, os, subprocess
binary = os.environ.get("MIYU_HOST_BIN")
token = os.environ.get("MIYU_HOST_TOKEN")
out = {"env_ok": bool(binary and token), "caps": os.environ.get("MIYU_HOST_CAPABILITIES")}
if binary and token:
    for method in ("providers.list", "host.info", "subsystems.enabled"):
        r = subprocess.run([binary, "host", method], capture_output=True, text=True)
        try:
            reply = json.loads(r.stdout or "{}")
        except ValueError:
            reply = {"raw": r.stdout, "stderr": r.stderr}
        out[method] = {"rc": r.returncode, "reply": reply}
print(json.dumps(out, ensure_ascii=False))
'''


def build_home():
    for path in (HOME, RUN, OUT):
        if path.exists():
            shutil.rmtree(path)
    (HOME / "config").mkdir(parents=True)
    RUN.mkdir(parents=True)
    OUT.mkdir(parents=True)
    cfg = persona_ab.load_real_config()
    for key in ("platforms", "web", "voice", "alarm"):
        cfg.pop(key, None)
    cfg["providers"] = [{
        "enabled": True, "id": "stub", "display_name": "Stub",
        "base_url": f"http://127.0.0.1:{STUB_PORT}/v1", "protocol": "openai-chat",
        "api_key": "stub-key-SECRET", "models": ["stub-a"],
    }]
    cfg["active_provider_models"] = [{"provider_id": "stub", "model": "stub-a"}]
    cfg.pop("active_multimodal_provider_models", None)
    cfg.setdefault("prompt", {})["active_persona"] = ""
    cfg.setdefault("memory", {})["association_enabled"] = False
    cfg.setdefault("cache", {})["request_log"] = False
    (HOME / "config" / "config.jsonc").write_text(json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8")
    # 放老布局的 data/scripts:daemon 首次启动会把它迁到 extensions/scripts(新布局);
    # 两处都放会让迁移撞上「目标已存在」而拒启。
    scripts_dir = HOME / "data" / "scripts"
    scripts_dir.mkdir(parents=True, exist_ok=True)
    path = scripts_dir / "host_probe.py"
    path.write_text(SCRIPT, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def env():
    e = dict(os.environ)
    e["MIYU_HOME"] = str(HOME)
    e["XDG_RUNTIME_DIR"] = str(RUN)
    for key in ("MIYU_DIRECT", "MIYU_SESSION", "MIYU_TURN_MODE", "MIYU_HOST_TOKEN", "MIYU_HOST_BIN",
                "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"):
        e.pop(key, None)
    e["LANG"] = "zh_CN.UTF-8"
    return e


def find_socket():
    for p in RUN.rglob("*.sock"):
        return p
    return None


results = []


def check(name, ok, detail=""):
    results.append({"name": name, "ok": bool(ok), "detail": str(detail)[:400]})
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  -- {str(detail)[:200]}"))


def main():
    assert MIYU.exists(), f"missing binary {MIYU}; run cargo build"
    build_home()
    stub = subprocess.Popen([sys.executable, str(BASE / "stub_llm.py")],
                            env=dict(os.environ, STUB_PORT=str(STUB_PORT), STUB_LOG=str(OUT / "stub.jsonl")),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    daemon = subprocess.Popen([str(MIYU), "daemon", "--port", str(PORT)], env=env(),
                              stdout=(OUT / "daemon.log").open("w"), stderr=subprocess.STDOUT)
    try:
        for _ in range(60):
            if find_socket():
                break
            time.sleep(0.5)
        assert find_socket(), "daemon socket never appeared"
        time.sleep(1.5)
        # 1. 非 daemon 进程直接跑 `miyu host`:没有令牌 → permission_denied,退出码 1。
        proc = subprocess.run([str(MIYU), "host", "providers.list"], env=env(), capture_output=True, text=True, timeout=30)
        reply = json.loads(proc.stdout or "{}")
        check("无令牌直接调用被拒", proc.returncode == 1 and reply.get("error", {}).get("code") == "permission_denied", proc.stdout[:200])
        # 2. 一整回合:桩模型调 host_probe,脚本在 daemon 里拿到令牌并查三个方法。
        proc = subprocess.run([str(MIYU), "ask", "--output-format", "json", "HOSTPROBE 查一下宿主"], env=env(),
                              capture_output=True, text=True, timeout=120)
        (OUT / "ask.stdout").write_text(proc.stdout, encoding="utf-8")
        (OUT / "ask.stderr").write_text(proc.stderr, encoding="utf-8")
        done = {}
        for line in proc.stdout.splitlines():
            line = line.strip()
            if line:
                done = json.loads(line)
        check("回合完成(done 帧)", proc.returncode == 0 and done.get("type") == "done", f"rc={proc.returncode} err={proc.stderr[:200]}")
        text = done.get("text", "")
        try:
            tool_result = json.loads(text)
            probe = json.loads(tool_result.get("stdout", "")) if isinstance(tool_result, dict) else {}
        except ValueError:
            tool_result, probe = {}, {}
        (OUT / "probe.json").write_text(json.dumps(probe, ensure_ascii=False, indent=2), encoding="utf-8")
        check("脚本拿到了令牌与授权集", probe.get("env_ok") is True and probe.get("caps") == "providers.read,host.info", probe)
        providers = probe.get("providers.list", {})
        listing = providers.get("reply", {}).get("data", {})
        ids = [p.get("id") for p in listing.get("providers", [])]
        check("providers.list 成功且含 stub", providers.get("rc") == 0 and providers.get("reply", {}).get("ok") is True and "stub" in ids, providers)
        check("providers.list 不带 api key / 地址", "SECRET" not in json.dumps(listing) and "base_url" not in json.dumps(listing) and "api_key" not in json.dumps(listing), listing)
        check("providers.list 标出激活选择", listing.get("active") == [{"provider_id": "stub", "model": "stub-a"}], listing.get("active"))
        info = probe.get("host.info", {})
        contracts = {c.get("id"): c.get("version") for c in info.get("reply", {}).get("data", {}).get("contracts", [])}
        check("host.info 报契约版本与授权集", info.get("rc") == 0 and contracts.get("scripts") == 1
              and info.get("reply", {}).get("data", {}).get("capabilities") == ["providers.read", "host.info"], info)
        subsystems = probe.get("subsystems.enabled", {})
        check("subsystems.enabled 未授权 → permission_denied", subsystems.get("rc") == 1
              and subsystems.get("reply", {}).get("error", {}).get("code") == "permission_denied", subsystems)
        check("整段 stdout 不泄漏密钥", "SECRET" not in proc.stdout)
    finally:
        subprocess.run([str(MIYU), "daemon", "stop"], env=env(), capture_output=True, timeout=30)
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
        stub.terminate()
        (OUT / "verdict.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    passed = sum(1 for r in results if r["ok"])
    print(f"\n{passed}/{len(results)} passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()

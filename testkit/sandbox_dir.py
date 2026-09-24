"""测具沙箱的共用建法：集中放在一个根下，跑完即删，残留一天后自动清掉。

09-24：二十来个测具各自 `tempfile.mkdtemp(prefix="miyu-…")` 在 /tmp 建沙箱（整份 Miyu
家目录）、跑完不删，攒到 1390 个、2.8G。/tmp 是内存盘，按用户有配额（约 24.8G），满了
之后这个用户下所有程序写 /tmp 都报 EDQUOT。

- 沙箱建在 `/tmp/miyu-testkit/` 下（`MIYU_TESTKIT_TMP` 可换）。路径还够短，里面的
  套接字路径不会超过 SUN_LEN（macOS 104 字节）。
- 进程退出时删掉。要留着现场排查就设 `MIYU_KEEP_SANDBOX=1`。产物（截屏、报告）别放
  沙箱里，放 `~/.cache/…`。
- 被 `timeout` 杀掉时 atexit 不跑（SIGTERM 默认直接终止进程），所以每次新建时顺手清掉
  这个根下放了一天以上的残留。

用法：

    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    import sandbox_dir  # noqa: E402
    home = sandbox_dir.make("miyu-effort-menu-")
"""

import atexit
import os
import shutil
import tempfile
import time
from pathlib import Path

ROOT = Path(os.environ.get("MIYU_TESTKIT_TMP", "/tmp/miyu-testkit"))
STALE_AFTER_SECONDS = 24 * 3600


def _prune_stale():
    now = time.time()
    try:
        entries = list(ROOT.iterdir())
    except OSError:
        return
    for entry in entries:
        try:
            stale = now - entry.stat().st_mtime > STALE_AFTER_SECONDS
        except OSError:
            continue
        if stale:
            shutil.rmtree(entry, ignore_errors=True)


def make(prefix, *, delete_at_exit=True):
    """建一个沙箱目录，返回它的路径。

    `delete_at_exit=False` 留给自己管去留的测具（比如红了要留现场的）：不在退出时删，
    但一天以后照样会被下一次新建清掉。
    """
    ROOT.mkdir(parents=True, exist_ok=True)
    _prune_stale()
    path = Path(tempfile.mkdtemp(prefix=prefix, dir=ROOT))
    if delete_at_exit and not os.environ.get("MIYU_KEEP_SANDBOX"):
        atexit.register(shutil.rmtree, path, True)
    return path

"""Owned, short-lived product test data. Never adopt an existing home."""
import json
import os
from pathlib import Path
import shutil
import stat
import tempfile
import uuid


class OwnershipError(RuntimeError):
    pass


def mock_config():
    """No inherited provider. Port 9 fails locally until a suite owns a mock.

    配置里 active_provider 是必填项,不带凭据的验收(macOS 预览)也用这一份。"""
    return {'config_version': 3, 'oobe_done': True, 'active_provider': 'distribution-mock',
            'active_provider_models': [{'provider_id': 'distribution-mock', 'model': 'mock'}],
            'providers': [{'id': 'distribution-mock', 'display_name': 'Local test',
                'base_url': 'http://127.0.0.1:9/v1', 'protocol': 'openai-chat',
                'api_key': 'local-test-only', 'models': ['mock']}],
            'memory': {'enabled': False}}


class Sandbox:
    def __init__(self):
        self.run_id = uuid.uuid4().hex
        self.root = Path(tempfile.mkdtemp(prefix='my-', dir='/tmp'))
        self.root.chmod(0o700)
        info = self.root.lstat()
        self.identity = (info.st_dev, info.st_ino, info.st_uid)
        self.marker = self.root / '.distribution-owner.json'
        self.owner = {'schema_version': 1, 'run_id': self.run_id,
                      'pid': os.getpid(), 'uid': os.getuid()}
        self.marker.write_text(json.dumps(self.owner), encoding='utf-8')
        self.marker.chmod(0o600)
        for name in ('miyu', 'home', 'runtime', 'cache', 'config', 'data', 'state', 'work', 'tmp'):
            (self.root / name).mkdir(mode=0o700)
        config = self.root / 'miyu/config'
        config.mkdir(mode=0o700)
        (config / 'config.jsonc').write_text(json.dumps(mock_config()), encoding='utf-8')

    def environment(self, inherited=None):
        inherited = dict(os.environ if inherited is None else inherited)
        # Allowlist preserves build tooling without provider secrets or resource overrides.
        # MIYU_LANG 决定界面文案是中文还是英文，而一批用例断言的正是中文那份。
        # 它不指向任何资源、也不是凭据，挡掉它只会让「跑测试的机器碰巧是什么
        # locale」决定成败——2026-09-21 CI 第一次真跑就栽在这儿：作业里设了
        # MIYU_LANG=zh，被这张白名单滤掉，runner 上退回英文，11 条 TUI 用例全红。
        names = {'PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'MIYU_LANG', 'TZ', 'TERM', 'COLORTERM',
                 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN', 'CARGO_TARGET_DIR',
                 'CARGO_BUILD_JOBS', 'RUSTC_WRAPPER', 'CC', 'CXX', 'AR', 'PKG_CONFIG_PATH',
                 'CARGO_NET_OFFLINE', 'SSL_CERT_FILE', 'SSL_CERT_DIR',
                 'HTTP_PROXY', 'HTTPS_PROXY', 'NO_PROXY', 'http_proxy', 'https_proxy', 'no_proxy'}
        env = {key: value for key, value in inherited.items() if key in names}
        original_home = Path(inherited.get('HOME', str(Path.home())))
        env.setdefault('CARGO_HOME', str(original_home / '.cargo'))
        env.setdefault('RUSTUP_HOME', str(original_home / '.rustup'))
        env.setdefault('PATH', os.defpath)
        env.setdefault('CARGO_BUILD_JOBS', '2')
        env.update(HOME=str(self.root/'home'), MIYU_HOME=str(self.root/'miyu'),
                   TMPDIR=str(self.root/'tmp'), XDG_RUNTIME_DIR=str(self.root/'runtime'),
                   XDG_CACHE_HOME=str(self.root/'cache'), XDG_CONFIG_HOME=str(self.root/'config'),
                   XDG_DATA_HOME=str(self.root/'data'), XDG_STATE_HOME=str(self.root/'state'))
        return env

    def cleanup(self):
        info = self.root.lstat()
        if (not stat.S_ISDIR(info.st_mode)
                or (info.st_dev, info.st_ino, info.st_uid) != self.identity):
            raise OwnershipError('Test root identity changed. Refusing cleanup.')
        if self.marker.is_symlink():
            raise OwnershipError('Ownership marker is a symlink. Refusing cleanup.')
        try:
            owner = json.loads(self.marker.read_text(encoding='utf-8'))
        except (OSError, ValueError) as error:
            raise OwnershipError('Ownership marker is missing or invalid.') from error
        if owner != self.owner:
            raise OwnershipError('Ownership marker mismatch. Refusing cleanup.')
        # Python uses fd-relative, symlink-resistant rmtree on supported Unix hosts.
        if not shutil.rmtree.avoids_symlink_attacks:
            raise OwnershipError('Safe directory cleanup is unavailable.')
        shutil.rmtree(self.root)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.cleanup()

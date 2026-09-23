"""macOS installation acceptance on the host itself (09-23, Homebrew channel).

Linux 在干净容器里装包；macOS 没有容器，改成两段：
  1. 把 tar.gz 解到一个带空格的临时前缀，跑同一个 probes/installed.py（逐文件哈希、版本、
     真模型回一句），外加三条只在 macOS 开的功能检查——资源按前缀找得到、技能加载得出、
     出厂脚本跑得起来。这正是 Homebrew 把包放进 Cellar 后的处境。
  2. homebrew-formula：用这次的包渲染 formula（file:// 地址），真 `brew install` 再 `brew test`。
     它会往本机 Homebrew 装依赖，所以只在一次性的 CI runner 上跑，个人 Mac 上默认拒绝。
"""
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys

from .common import BlockedError, write_json
from . import homebrew
from .isolation import mock_config

PROBE = Path(__file__).resolve().parents[1]/'probes/installed.py'
TAP = 'miyu-verify/local'
BREW_ENV = {'HOMEBREW_NO_AUTO_UPDATE': '1', 'HOMEBREW_NO_INSTALL_CLEANUP': '1',
            'HOMEBREW_NO_ANALYTICS': '1', 'HOMEBREW_NO_ENV_HINTS': '1',
            'HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK': '1'}


def version_tuple(text):
    return tuple(int(part) for part in text.split('.'))


def host_facts(manifest):
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise BlockedError('macOS installation acceptance must run on an Apple Silicon Mac.')
    system = subprocess.run(['sw_vers', '-productVersion'], check=True, capture_output=True,
                            text=True, timeout=30).stdout.strip()
    minimum = manifest['builders']['macos-arm64']['minimum_test_os']
    if version_tuple(system) < version_tuple(minimum):
        raise BlockedError(f'macOS {system} is older than the supported minimum {minimum}.')
    build = subprocess.run(['sw_vers', '-buildVersion'], check=True, capture_output=True,
                           text=True, timeout=30).stdout.strip()
    return {'kind': 'macos-native', 'architecture': 'arm64', 'system': system,
            'system_build': build, 'runner_image': ' '.join(filter(None, (
                os.environ.get('ImageOS'), os.environ.get('ImageVersion'))))}


class Session:
    """Run commands, keep redacted logs and the command list the report publishes."""

    def __init__(self, out, secret):
        self.out, self.secret, self.commands = out, secret, []

    def run(self, argv, log_name, timeout=600, env=None, check=True):
        argv = [str(arg) for arg in argv]
        result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, env=env,
                                stdin=subprocess.DEVNULL)
        log = result.stdout+result.stderr
        if self.secret:
            log = log.replace(self.secret, '[REDACTED]')
        (self.out/log_name).write_text(log, encoding='utf-8')
        self.commands.append({'command': argv, 'exit_code': result.returncode, 'log': log_name})
        if check and result.returncode:
            raise ValueError(f'macOS check failed. See {log_name}.')
        return result.stdout


def extract_and_probe(session, box, manifest, asset_id, record, package, config, live):
    """Stage 1: the relocated prefix. Returns the probe's final JSON line."""
    prefix = box.root/'Miyu Tar Test'  # 空格是故意的:Cellar 路径没有,别人的前缀会有
    prefix.mkdir()
    session.run(['tar', '-xzf', package, '-C', prefix], asset_id+'-extract.txt', 120)
    home = box.root/'probes'/asset_id
    for child in ('home', 'miyu/config', 'runtime', 'config', 'data', 'cache', 'state'):
        (home/child).mkdir(parents=True, mode=0o700, exist_ok=True)
    write_json(home/'miyu/config/config.jsonc', config)
    (home/'miyu/config/config.jsonc').chmod(0o600)
    argv = [sys.executable, PROBE, '--record', package.parent/'package-record.json',
            '--prefix', prefix, '--version', manifest['version'],
            '--revision', manifest['package_revision'], '--fedora-version', manifest['fedora_version'],
            '--test-home', home, '--uid', os.getuid(), '--gid', os.getgid(), '--functional']
    if not live:
        argv.append('--skip-provider')
    output = session.run(argv, asset_id+'-probe.txt', 420)
    return json.loads(output.strip().splitlines()[-1])


def homebrew_install(session, manifest, source, record, package, box, allowed):
    """Stage 2: the formula users will run, against this exact tarball."""
    if not allowed:
        raise BlockedError('homebrew-formula installs dependencies into this Mac\'s Homebrew. Run it '
                           'on a disposable CI runner or pass --allow-homebrew-changes.')
    brew = shutil.which('brew') or '/opt/homebrew/bin/brew'
    env = dict(os.environ, **BREW_ENV)
    formula = homebrew.render_formula((source/homebrew.FORMULA).read_text(),
        version=manifest['version'], package_revision=manifest['package_revision'],
        url='file://'+str(package.resolve()), sha256=record['sha256'])
    session.run([brew, 'tap-new', '--no-git', TAP], 'brew-tap-new.txt', 120, env)
    try:
        tap_dir = Path(session.run([brew, '--repository', TAP], 'brew-repository.txt', 60, env).strip())
        (tap_dir/'Formula').mkdir(exist_ok=True)
        (tap_dir/'Formula/miyu.rb').write_text(formula)
        session.run([brew, 'install', f'{TAP}/miyu'], 'brew-install.txt', 2400, env)
        session.run([brew, 'test', f'{TAP}/miyu'], 'brew-test.txt', 600, env)
        keg = Path(session.run([brew, '--prefix', f'{TAP}/miyu'], 'brew-keg.txt', 60, env).strip())
        linked = Path(session.run([brew, '--prefix'], 'brew-prefix.txt', 60, env).strip())/'bin/miyu'
        probe_env = box.environment()
        # 用户敲的就是 brew 链出来的那个入口,资源要顺着它找到 Cellar 里的 share/miyu。
        version = session.run([linked, '--version'], 'brew-miyu-version.txt', 60, probe_env).strip()
        paths = session.run([linked, 'paths'], 'brew-miyu-paths.txt', 60, probe_env)
        personas = next((line.split(': ', 1)[1] for line in paths.splitlines()
                         if line.startswith('system persona resources: ')), '')
        if version != f'miyu {manifest["version"]}':
            raise ValueError(f'Homebrew-installed binary reports {version!r}.')
        if not personas or Path(personas).resolve() != (keg/'share/miyu/personas').resolve():
            raise ValueError(f'Homebrew install resolves persona resources to {personas!r}.')
        return {'keg': str(keg.resolve()), 'version': version, 'persona_resources': personas}
    finally:
        session.run([brew, 'uninstall', '--formula', f'{TAP}/miyu'], 'brew-uninstall.txt', 300, env,
                    check=False)
        session.run([brew, 'untap', TAP], 'brew-untap.txt', 120, env, check=False)
        # 全名安装会自动记一条信任(Homebrew 6),tap 删了它也还在。
        session.run([brew, 'untrust', '--formula', f'{TAP}/miyu'], 'brew-untrust.txt', 60, env,
                    check=False)


def homebrew_remaining():
    brew = shutil.which('brew') or '/opt/homebrew/bin/brew'
    if not Path(brew).exists():
        return ''
    listed = subprocess.run([brew, 'list', '--formula', '--full-name'], capture_output=True, text=True,
                            timeout=120, env=dict(os.environ, **BREW_ENV)).stdout.split()
    return ' '.join(name for name in listed if name.startswith(TAP+'/'))


def verify_macos(args, manifest, source, required, records, provider, out, box):
    """Return (checks, commands, results, host). Every required check gets an explicit status."""
    session = Session(out, provider['api_key'] if provider else None)
    host = host_facts(manifest)
    config = mock_config()
    if provider:
        config.update(active_provider='opencodego', providers=[provider],
                      active_provider_models=[{'provider_id': 'opencodego', 'model': 'deepseek-v4.1-flash'}])
    status, results, brew_ran = {}, {}, False
    for asset_id, record in records.items():
        package = args.packages/asset_id/record['asset']['filename']
        wanted = {c['check'] for c in required if c['asset_id'] == asset_id}
        try:
            results[asset_id] = extract_and_probe(session, box, manifest, asset_id, record, package,
                                                  config, provider is not None)
            for check in ('artifact-identity', 'package-install', 'assets-complete'):
                status[asset_id, check] = ('PASS', None)
            status[asset_id, 'provider-live'] = (('PASS', None) if provider else
                                                 ('SKIPPED', 'No provider configuration was supplied.'))
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            for check in ('artifact-identity', 'package-install', 'assets-complete', 'provider-live'):
                status[asset_id, check] = ('FAIL', str(error))
        if 'homebrew-formula' in wanted:
            try:
                brew_ran = args.allow_homebrew_changes
                brew = homebrew_install(session, manifest, source, record, package, box,
                                        args.allow_homebrew_changes)
                results.setdefault(asset_id, {})['homebrew'] = brew
                status[asset_id, 'homebrew-formula'] = ('PASS', None)
            except BlockedError as error:
                status[asset_id, 'homebrew-formula'] = ('SKIPPED', str(error))
            except (ValueError, OSError, subprocess.SubprocessError) as error:
                status[asset_id, 'homebrew-formula'] = ('FAIL', str(error))
    remaining = homebrew_remaining() if brew_ran else ''
    write_json(out/'cleanup.json', {'kind': 'native', 'remaining': remaining})
    if remaining:
        status = {key: ('FAIL', 'Homebrew verification tap was not removed: '+remaining)
                  for key in status}
    checks = []
    for check in required:
        state, reason = status.get((check['asset_id'], check['check']),
                                   ('FAIL', 'Check was not executed.'))
        entry = dict(check, status=state, artifact_sha256=records[check['asset_id']]['sha256'])
        if reason:
            entry['reason'] = reason.replace(session.secret, '[REDACTED]') if session.secret else reason
        checks.append(entry)
    return checks, session.commands, results, host

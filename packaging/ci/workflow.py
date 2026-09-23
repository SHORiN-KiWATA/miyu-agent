#!/usr/bin/env python3
"""Small, controlled GitHub Actions adapters for the release scripts (Linux containers, macOS runner)."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

from lib.common import BlockedError, fresh_directory, load_json, sha256_file, write_json
from lib.downloads import safe_extract
from lib.inputs import verify_prepared
from lib.source import git, safe_relative
from prepare import verify_source
from verify import local_provider

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = Path(__file__).resolve().parent
BUILD_TARGETS = {
    'arch-x86_64': ('arch-x86_64',),
    'gnu-x86_64': ('debian13-x86_64', 'ubuntu2404-x86_64', 'ubuntu2604-x86_64',
                   'mint22-x86_64', 'fedora-current-x86_64'),
}
# macOS 包在 GitHub 的 macos runner 上构建(09-23 用户拍板)。发版链的其余部分仍在本机,
# 所以 runner 从同一个提交重新冻结一次,本机再核对两边 release-input 逐字节相同。
MACOS_PROFILE = 'smoke'
MACOS_BUILD = 'macos-arm64'


def run(script, *arguments, timeout=9000):
    return subprocess.run([sys.executable, str(SCRIPTS/script), *map(str, arguments)],
                          cwd=REPO, check=True, timeout=timeout)


def provider_configuration():
    value = os.environ.get('OPENCODEGO_PROVIDER_CONFIG', '')
    if not value:
        raise BlockedError('OPENCODEGO_PROVIDER_CONFIG is required for real provider acceptance.')
    config = json.loads(value)
    if (not isinstance(config, dict) or set(config) != {'providers'}
            or not isinstance(config['providers'], list) or len(config['providers']) != 1
            or not isinstance(config['providers'][0], dict)
            or config['providers'][0].get('id') != 'opencodego'):
        raise ValueError('Dedicated provider secret must contain exactly one opencodego provider.')
    provider = config['providers'][0]
    allowed = ('id', 'display_name', 'base_url', 'protocol', 'api_key', 'models')
    provider = {key: provider[key] for key in allowed if key in provider}
    if (not isinstance(provider.get('api_key'), str) or not provider['api_key']
            or 'deepseek-v4.1-flash' not in provider.get('models', [])):
        raise ValueError('Dedicated provider secret needs a key and deepseek-v4.1-flash.')
    return {'providers': [provider]}


def plan(args):
    version = tomllib.loads((REPO/'Cargo.toml').read_text())['package']['version']
    if args.revision <= 0:
        raise ValueError('Package revision must be positive.')
    output = fresh_directory(args.out)
    write_json(output/'dry-run-plan.json', {
        'schema_version': 1, 'status': 'NOT_EXECUTED', 'dry_run': True,
        'source_commit': git(REPO, 'rev-parse', 'HEAD'), 'version': version,
        'profile': 'linux-smoke', 'requested_tag': args.tag or 'v'+version,
        'package_revision': args.revision, 'builds': BUILD_TARGETS,
        'steps': ['metadata', 'prepare', 'native GNU/Arch container build', 'stage',
                  'package', 'five real installation/provider checks', 'aggregate',
                  'publish dry-run', 'publish execute'],
        'required_secret': 'OPENCODEGO_PROVIDER_CONFIG',
        'execution_requirements': ['Existing version tag equals the workflow source commit',
                                   'Dedicated provider secret is configured',
                                   'All five target reports pass against the final package hashes'],
        'claims': 'Planning only. No compilation, installation check or publication was executed.'})
    print('DRY RUN PLAN ONLY: no build, acceptance report or remote publication was executed.')


def default_download_cache():
    """机器级下载缓存:跟发版 root 无关,换一个 root 也命中。"""
    root = os.environ.get('XDG_CACHE_HOME') or (Path.home()/'.cache')
    return Path(root)/'miyu-release'/'downloads'


def freeze(args):
    version = tomllib.loads((REPO/'Cargo.toml').read_text())['package']['version']
    if args.tag != 'v'+version or args.revision <= 0:
        raise ValueError('Execution requires the exact version tag and a positive revision.')
    head = git(REPO, 'rev-parse', 'HEAD')
    if git(REPO, 'rev-parse', '--verify', 'refs/tags/'+args.tag+'^{commit}') != head:
        raise ValueError('The existing release tag must equal the workflow source commit.')
    if os.environ.get('GITHUB_SHA', head) != head:
        raise ValueError('Checkout differs from the controlling workflow commit.')
    run('metadata.py', '--mode', 'release', '--source-ref', head, '--profile', 'linux-smoke',
        '--revision', args.revision, '--tag', args.tag, '--out', args.root/'release-input.json')
    # 锁定归档按哈希取,同一份字节跨次复用是安全的:命中一侧照样逐字节重算。
    # 不给缓存的话每发一版就重下一遍(光语音模型三件就约 190MB,09-21 实测)。
    run('prepare.py', '--manifest', args.root/'release-input.json', '--out', args.root/'inputs',
        '--cache', args.download_cache or default_download_cache())


def build_packages(args):
    root = args.root.resolve()
    manifest, source = verify_source(root/'release-input.json')
    verify_prepared(root/'inputs', manifest, root/'release-input.json', require_vendor=True)
    if manifest['profile'] != 'linux-smoke' or args.build_id not in manifest['builds']:
        raise ValueError('This controlled workflow only builds the frozen Linux smoke profile.')
    output = fresh_directory(args.out)
    family = {'gnu-x86_64': 'gnu', 'arch-x86_64': 'arch'}[args.build_id]
    dockerfile = source/f'packaging/linux/builders/Dockerfile.{family}'
    iid = output/'builder-image.id'
    subprocess.run(['docker', 'build', '--platform', 'linux/amd64', '--iidfile', str(iid),
                    '--build-arg', 'RUST_VERSION='+manifest['toolchain']['rust'],
                    '-f', str(dockerfile), str(source)], check=True, timeout=2400)
    image = iid.read_text().strip()
    try:
        for component in manifest['builds'][args.build_id]['components']:
            run('build.py', '--manifest', root/'release-input.json', '--inputs', root/'inputs',
                '--build-id', args.build_id, '--component', component, '--builder-image', image,
                '--target-cache', output/'targets'/component, '--out', output/'build'/component)
        run('stage.py', '--manifest', root/'release-input.json', '--inputs', root/'inputs',
            '--build-id', args.build_id, '--build-root', output/'build', '--out', output/'stage')
        for asset in manifest['assets']:
            if asset['build_id'] == args.build_id:
                run('package.py', '--manifest', root/'release-input.json', '--stage', output/'stage',
                    '--asset-id', asset['id'], '--out', output/'packages'/asset['id'],
                    '--nfpm', root/'inputs/tools/nfpm/nfpm', '--builder-image', image)
    finally:
        subprocess.run(['docker', 'image', 'rm', image], check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)


def verify_packages(args):
    root = args.root.resolve()
    manifest, _ = verify_source(root/'release-input.json')
    if manifest['profile'] != 'linux-smoke':
        raise ValueError('Live CI acceptance only supports the Linux smoke profile.')
    config = provider_configuration()
    # The dedicated credential never enters the artifact/output tree.
    with tempfile.TemporaryDirectory(prefix='miyu-ci-provider-') as temporary:
        path = Path(temporary)/'provider.json'
        write_json(path, config)
        path.chmod(0o600)
        local_provider(path)
        for target in BUILD_TARGETS[args.build_id]:
            if target not in manifest['targets']:
                raise ValueError('A required Linux installation target is absent.')
            run('verify.py', '--manifest', root/'release-input.json', '--packages', args.out/'packages',
                '--target-id', target, '--report-dir', args.out/'reports'/target,
                '--provider-config', path)


def macos_parameters(event, ref_name, inputs):
    """Candidate branches carry the release request in their name: release/v0.6.3-1.

    workflow_dispatch 只认默认分支上已有的工作流文件;推候选分支触发则跑分支自己的那份,
    发版时不用先把工作流合进 main。macos-preview/** 分支只出预览包(不打 tag)。"""
    if event == 'push' and ref_name.startswith('release/'):
        match = re.fullmatch(r'release/(v\d+\.\d+\.\d+)-([1-9]\d*)', ref_name)
        if not match:
            raise ValueError('Release candidate branches are named release/v<version>-<revision>.')
        return {'mode': 'release', 'tag': match.group(1), 'revision': match.group(2), 'expect': ''}
    if event == 'push' and ref_name.startswith('macos-preview/'):
        return {'mode': 'preview', 'tag': '', 'revision': '1', 'expect': ''}
    if event == 'workflow_dispatch':
        mode, tag, revision = inputs.get('mode', ''), inputs.get('tag', ''), inputs.get('revision', '1')
        expect = inputs.get('expect', '')
        if (mode not in ('preview', 'release') or not re.fullmatch(r'[1-9]\d*', revision)
                or (mode == 'release') != bool(re.fullmatch(r'v\d+\.\d+\.\d+', tag))
                or (expect and not re.fullmatch(r'[0-9a-f]{64}', expect))):
            raise ValueError('Invalid macOS package dispatch inputs.')
        return {'mode': mode, 'tag': tag, 'revision': revision, 'expect': expect}
    raise ValueError(f'Unsupported macOS package trigger: {event} {ref_name}')


def macos_params(args):
    values = macos_parameters(os.environ.get('GITHUB_EVENT_NAME', ''), os.environ.get('GITHUB_REF_NAME', ''),
        {key: os.environ.get('INPUT_'+key.upper(), '') for key in ('mode', 'tag', 'revision', 'expect')})
    print(''.join(f'{key}={value}\n' for key, value in values.items()), end='')


def macos_freeze(args):
    version = tomllib.loads((REPO/'Cargo.toml').read_text())['package']['version']
    command = ['--mode', args.mode, '--source-ref', 'HEAD', '--profile', MACOS_PROFILE,
               '--revision', args.revision, '--out', args.root/'release-input.json']
    if args.mode == 'release':
        if args.tag != 'v'+version or args.revision <= 0:
            raise ValueError('Release mode requires the exact version tag and a positive revision.')
        head = git(REPO, 'rev-parse', 'HEAD')
        existing = subprocess.run(['git', '-C', str(REPO), 'rev-parse', '--verify', '--quiet',
                                   'refs/tags/'+args.tag+'^{commit}'], capture_output=True, text=True)
        if existing.returncode == 0 and existing.stdout.strip() != head:
            raise ValueError('The release tag exists but points at another commit.')
        if existing.returncode != 0:
            # 候选分支推上来时 tag 还没公开(手册:验收完才推 tag)。只在一次性 runner 上补一个
            # 本地轻量 tag,好让 metadata 写出与本机完全相同的 tag/tag_commit。
            if os.environ.get('GITHUB_ACTIONS') != 'true':
                raise ValueError('Create the release tag locally before freezing.')
            subprocess.run(['git', '-C', str(REPO), 'tag', args.tag, head], check=True, timeout=30)
        command += ['--tag', args.tag]
    run('metadata.py', *command)
    digest = sha256_file(args.root/'release-input.json')
    print(f'release-input.json sha256: {digest}')
    if args.expect_release_input and args.expect_release_input != digest:
        raise ValueError(f'Runner froze a different release input than expected: {digest}')
    run('prepare.py', '--manifest', args.root/'release-input.json', '--out', args.root/'inputs')


def macos_build(args):
    root = args.root.resolve()
    manifest = root/'release-input.json'
    output = fresh_directory(args.out)
    run('build.py', '--manifest', manifest, '--inputs', root/'inputs', '--build-id', MACOS_BUILD,
        '--component', 'core', '--target-cache', output/'targets/core', '--out', output/'build/core')
    run('stage.py', '--manifest', manifest, '--inputs', root/'inputs', '--build-id', MACOS_BUILD,
        '--build-root', output/'build', '--out', output/'stage')
    run('package.py', '--manifest', manifest, '--stage', output/'stage', '--asset-id', 'macos-core',
        '--out', output/'packages/macos-core')


def macos_verify(args):
    root = args.root.resolve()
    command = ['--manifest', root/'release-input.json', '--packages', args.out/'packages',
               '--target-id', MACOS_BUILD, '--report-dir', args.out/'reports'/MACOS_BUILD,
               '--allow-homebrew-changes']
    if not os.environ.get('OPENCODEGO_PROVIDER_CONFIG'):
        if args.require_provider:
            raise BlockedError('OPENCODEGO_PROVIDER_CONFIG is required for release acceptance.')
        # 预览:除了没调模型的 provider-live 记 SKIPPED,其余检查必须全过。
        subprocess.run([sys.executable, str(SCRIPTS/'verify.py'), *map(str, command), '--skip-provider'],
                       cwd=REPO, timeout=9000)
        report = load_json(args.out/'reports'/MACOS_BUILD/'report.json')
        failed = [c['check'] for c in report['checks']
                  if c['status'] != 'PASS' and not (c['check'] == 'provider-live' and c['status'] == 'SKIPPED')]
        if failed:
            raise ValueError(f'macOS preview checks failed: {failed}')
        return None
    config = provider_configuration()
    with tempfile.TemporaryDirectory(prefix='miyu-ci-provider-') as temporary:
        path = Path(temporary)/'provider.json'
        write_json(path, config)
        path.chmod(0o600)
        local_provider(path)
        run('verify.py', *command, '--provider-config', path)


def macos_import(args):
    """Bring the runner's macOS package and report into the local release tree, byte-checked."""
    expected = sha256_file(args.manifest)
    with tempfile.TemporaryDirectory(prefix='miyu-macos-import-') as temporary:
        unpacked = Path(temporary)/'results'
        unpack_transport(args.archive, unpacked)
        record = load_json(unpacked/'packages/macos-core/package-record.json')
        report = load_json(unpacked/'reports'/MACOS_BUILD/'report.json')
        if record['release_input_sha256'] != expected or report['release_input_sha256'] != expected:
            raise ValueError('The runner built against a different release input. Re-freeze or re-run.')
        for kind, destination in (('packages/macos-core', args.packages/'macos-core'),
                                  ('reports/'+MACOS_BUILD, args.reports/MACOS_BUILD)):
            if destination.exists():
                raise ValueError(f'Refusing to overwrite {destination}.')
            shutil.copytree(unpacked/kind, destination, symlinks=True)
    print(f'Imported macOS package and report for release input {expected}.')


def bundle(args):
    root = args.root.resolve()
    if args.kind == 'diagnostics':
        members = tuple(p.relative_to(root).as_posix() for p in root.rglob('*.log')
                        if p.relative_to(root).parts[0] != 'reports')
        if (root/'reports').is_dir():
            members += ('reports',)
    else:
        members = {'frozen': ('release-input.json', 'source-files.json', 'source', 'inputs'),
               'results': ('packages', 'reports'),
               'publish': ('release-input.json', 'publish')}[args.kind]
    if args.out.exists():
        raise ValueError('Refusing to overwrite an existing transport archive.')
    args.out.parent.mkdir(parents=True, exist_ok=True)
    for name in members:
        if not (root/name).exists():
            raise ValueError(f'Transport input is incomplete: {name}')
    with tarfile.open(args.out, 'w:gz', compresslevel=1) as archive:
        for name in members:
            archive.add(root/name, arcname=name, recursive=True)


def unpack_transport(archive, destination):
    # Upstream extraction intentionally normalizes modes. Transport must retain
    # exact recorded modes (including real 0640 vendor files) across runner UIDs.
    safe_extract(archive, destination)
    with tarfile.open(archive, 'r:*') as stream:
        for member in stream:
            if member.mode & 0o7000:
                raise ValueError('Transport archive contains special permission bits.')
            if member.isfile() or member.isdir():
                (Path(destination)/member.name).chmod(member.mode & 0o777)


def aggregate(args):
    root = args.root.resolve()
    packages, reports = fresh_directory(root/'packages'), fresh_directory(root/'reports')
    for build_id in BUILD_TARGETS:
        unpacked = args.downloads/('unpacked-'+build_id)
        unpack_transport(args.downloads/('results-'+build_id+'.tar.gz'), unpacked)
        for kind, destination in [('packages', packages), ('reports', reports)]:
            for child in (unpacked/kind).iterdir():
                if not child.is_dir() or child.is_symlink() or (destination/child.name).exists():
                    raise ValueError('Duplicate or invalid aggregate artifact directory.')
                shutil.copytree(child, destination/child.name, symlinks=True)
    run('verify_release.py', '--manifest', root/'release-input.json', '--artifacts', packages,
        '--reports', reports, '--publish-dir', root/'publish')
    run('publish.py', '--manifest', root/'release-input.json', '--dir', root/'publish', '--dry-run')


def publish(args):
    notes = REPO/str(safe_relative(args.notes))
    if not notes.resolve().is_relative_to(REPO) or not notes.is_file():
        raise ValueError('Publication notes must be an existing file inside the checked-out repository.')
    run('publish.py', '--manifest', args.root/'release-input.json', '--dir', args.root/'publish',
        '--notes', notes, '--repository', os.environ['GITHUB_REPOSITORY'], '--execute')


def channels(args):
    manifest = load_json(args.root/'release-input.json')
    if manifest['source_commit'] != git(REPO, 'rev-parse', 'HEAD'):
        raise ValueError('Dispatch channel updates on the verified release source tag/ref.')
    output = args.root/'publish'/f'release-manifest-{manifest["version"]}-{manifest["package_revision"]}.json'
    command = ['--manifest', args.root/'release-input.json', '--release-output', output, '--out', args.out]
    if args.published_url:
        command += ['--published-url', args.published_url]
    with tempfile.TemporaryDirectory(prefix='miyu-ci-channel-builder-') as temporary:
        iid = Path(temporary)/'image.id'
        subprocess.run(['docker', 'build', '--platform', 'linux/amd64', '--iidfile', str(iid),
            '--build-arg', 'RUST_VERSION='+manifest['toolchain']['rust'], '-f',
            str(REPO/'packaging/linux/builders/Dockerfile.arch'), str(REPO)], check=True, timeout=2400)
        image = iid.read_text().strip()
        try:
            run('channel_update.py', *command, '--builder-image', image)
        finally:
            subprocess.run(['docker', 'image', 'rm', image], check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('plan', 'freeze'):
        command = commands.add_parser(name)
        command.add_argument('--tag', default='')
        command.add_argument('--revision', type=int, default=1)
        command.add_argument('--out' if name == 'plan' else '--root', type=Path, required=True)
        if name == 'freeze':
            command.add_argument('--download-cache', type=Path,
                                 help='Reusable locked-archive cache; defaults to '
                                      '$XDG_CACHE_HOME/miyu-release/downloads.')
    commands.add_parser('check-provider')
    for name in ('build', 'verify'):
        command = commands.add_parser(name)
        command.add_argument('--root', type=Path, required=True)
        command.add_argument('--out', type=Path, required=True)
        command.add_argument('--build-id', choices=tuple(BUILD_TARGETS), required=True)
    commands.add_parser('macos-params')
    command = commands.add_parser('macos-freeze')
    command.add_argument('--mode', choices=('preview', 'release'), required=True)
    command.add_argument('--tag', default='')
    command.add_argument('--revision', type=int, default=1)
    command.add_argument('--root', type=Path, required=True)
    command.add_argument('--expect-release-input', default='')
    for name in ('macos-build', 'macos-verify'):
        command = commands.add_parser(name)
        command.add_argument('--root', type=Path, required=True)
        command.add_argument('--out', type=Path, required=True)
        if name == 'macos-verify':
            command.add_argument('--require-provider', action='store_true')
    command = commands.add_parser('macos-import')
    command.add_argument('--archive', type=Path, required=True)
    command.add_argument('--manifest', type=Path, required=True)
    command.add_argument('--packages', type=Path, required=True)
    command.add_argument('--reports', type=Path, required=True)
    command = commands.add_parser('bundle')
    command.add_argument('--root', type=Path, required=True)
    command.add_argument('--kind', choices=('frozen', 'results', 'publish', 'diagnostics'), required=True)
    command.add_argument('--out', type=Path, required=True)
    command = commands.add_parser('unpack')
    command.add_argument('--archive', type=Path, required=True)
    command.add_argument('--out', type=Path, required=True)
    command = commands.add_parser('aggregate')
    command.add_argument('--root', type=Path, required=True)
    command.add_argument('--downloads', type=Path, required=True)
    command = commands.add_parser('publish')
    command.add_argument('--root', type=Path, required=True)
    command.add_argument('--notes', required=True)
    command = commands.add_parser('channels')
    command.add_argument('--root', type=Path, required=True)
    command.add_argument('--out', type=Path, required=True)
    command.add_argument('--published-url', default='')
    args = parser.parse_args()
    try:
        actions = {'plan': plan, 'freeze': freeze, 'build': build_packages, 'verify': verify_packages,
                   'bundle': bundle, 'aggregate': aggregate, 'publish': publish, 'channels': channels,
                   'macos-params': macos_params, 'macos-freeze': macos_freeze, 'macos-build': macos_build,
                   'macos-verify': macos_verify, 'macos-import': macos_import,
                   'unpack': lambda a: unpack_transport(a.archive, a.out),
                   'check-provider': lambda _: provider_configuration()}
        actions[args.command](args)
        return 0
    except BlockedError as error:
        print(f'BLOCKED: {error}', file=sys.stderr)
        return 3
    except (ValueError, KeyError, TypeError, OSError, tarfile.TarError, subprocess.SubprocessError) as error:
        print(f'ERROR: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())

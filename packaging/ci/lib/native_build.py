"""macOS native builds. macOS has no Docker, so only the isolation mechanism changes.

容器那条路守的三条照搬（09-23，Homebrew 渠道）：锁定的工具链、只用冻结的 vendor 离线编、
产物带可追溯的构建记录。另加两条 macOS 独有的：最低系统钉在锁文件的部署目标，二进制里
不许出现构建机的家目录——编译期那个源码根路径会被编进二进制，在谁的家目录下构建就会把
谁的用户名带进公开包。
"""
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile

from .common import BlockedError
from . import macho

# 相对 cargo 的工作目录（冻结源码树）。容器里是 /inputs/vendor；原生构建没有固定挂载点，
# 用相对路径让构建记录与机器无关。cargo 把 --config 里的相对路径按当前目录解析。
NATIVE_VENDOR = '../inputs/vendor'
# 这些变量会改变编译结果，构建机上残留什么都不能带进发布构建。
BUILD_ALTERING = ('RUSTFLAGS', 'RUSTDOCFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTC',
                  'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CC', 'CXX', 'AR', 'SDKROOT')


def is_native(build):
    return build['target'].endswith('-apple-darwin')


def binary_name(component):
    return 'miyu-voice' if component == 'voice' else 'miyu'


def cargo_command(target, component, features, vendor):
    command = ['cargo', 'build', '--release', '--frozen', '--target', target,
               '--bin', binary_name(component),
               '--config', 'source.crates-io.replace-with="vendored-sources"',
               '--config', f'source.vendored-sources.directory="{vendor}"']
    if features:
        command += ['--features', ','.join(features)]
    return command


def _output(argv, **kwargs):
    return subprocess.run(argv, check=True, capture_output=True, text=True, timeout=60, **kwargs).stdout.strip()


def host_facts(builder):
    """构建机的身份。原生构建没有镜像 digest，记录系统、Xcode、SDK 与编译器版本代替。"""
    try:
        xcode = _output(['xcodebuild', '-version']).splitlines()
    except (OSError, subprocess.SubprocessError):
        xcode = []  # 只有 Command Line Tools 的机器没有 xcodebuild，交给 require_host 报
    return {'kind': 'macos-native', 'architecture': platform.machine(),
            'system': _output(['sw_vers', '-productVersion']),
            'system_build': _output(['sw_vers', '-buildVersion']),
            'xcode': xcode[0].removeprefix('Xcode ').strip() if xcode else '',
            'sdk': 'macosx'+_output(['xcrun', '--sdk', 'macosx', '--show-sdk-version']),
            'clang': _output(['xcrun', 'clang', '--version']).splitlines()[0],
            'deployment_target': builder['deployment_target'],
            'runner_image': ' '.join(filter(None, (os.environ.get('ImageOS'),
                                                   os.environ.get('ImageVersion'))))}


def require_host(builder, facts):
    if platform.system() != 'Darwin' or facts['architecture'] != 'arm64':
        raise BlockedError('Native macOS build runner is required.')
    if facts['xcode'] != builder['xcode'] or facts['sdk'] != builder['sdk']:
        raise BlockedError(f'Select the locked Xcode {builder["xcode"]} ({builder["sdk"]}); '
                           f'found {facts["xcode"]} ({facts["sdk"]}).')


def require_outside_home(*paths):
    home = Path.home().resolve()
    for path in paths:
        if Path(path).resolve().is_relative_to(home):
            raise ValueError(f'Native build paths must live outside {home}: the source root '
                             f'is compiled into the binary. Move {path} under /tmp.')


def require_no_home_path(binary):
    home = str(Path.home().resolve()).encode()
    if home in Path(binary).read_bytes():
        raise ValueError(f'Built binary embeds the builder home directory {home.decode()}.')


def compile_native(manifest, build_id, component, source, inputs, out, target_dir, identity):
    """Build in place and return the compile record the container path writes to compile.json."""
    build = manifest['builds'][build_id]
    builder = manifest['builders'][build_id]
    facts = host_facts(builder)
    require_host(builder, facts)
    if Path(inputs).resolve() != (Path(source).parent/'inputs').resolve():
        raise ValueError('Native builds need prepared inputs beside the frozen source (../inputs).')
    require_outside_home(source, inputs, out, target_dir)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('CARGO_', 'MIYU_')) and key not in BUILD_ALTERING}
    env.update(CARGO_HOME=str(out/'cargo-home'), CARGO_TARGET_DIR=str(target_dir),
               CARGO_NET_OFFLINE='true', MIYU_BUILD_ID=identity,
               SOURCE_DATE_EPOCH=str(manifest['source_date_epoch']),
               MACOSX_DEPLOYMENT_TARGET=builder['deployment_target'],
               RUSTUP_TOOLCHAIN=manifest['toolchain']['rust'],
               SHERPA_ONNX_ARCHIVE_DIR=str(Path(inputs)/'archives'))
    (out/'cargo-home').mkdir(exist_ok=True)
    command = cargo_command(build['target'], component, build['features'][component], NATIVE_VENDOR)
    with (out/'build.log').open('wb') as log:
        subprocess.run(command, cwd=source, env=env, check=True, stdout=log,
                       stderr=subprocess.STDOUT, timeout=7200)
    name = binary_name(component)
    binary = out/name
    shutil.copy2(Path(target_dir)/build['target']/'release'/name, binary)
    macho.require_macos_arm64_executable(binary, builder['deployment_target'])
    signed = subprocess.run(['codesign', '--verify', '--strict', str(binary)],
                            capture_output=True, text=True, timeout=60)
    if signed.returncode:
        raise ValueError('Built binary has an invalid code signature: '+signed.stderr.strip())
    require_no_home_path(binary)
    # 套接字路径受 SUN_LEN(104) 限制，探测用的家目录放短路径。
    with tempfile.TemporaryDirectory(prefix='mv-', dir='/tmp') as home:
        probe = dict(env, HOME=home, MIYU_HOME=home, XDG_RUNTIME_DIR=home)
        version = subprocess.run([str(binary), '--version'], env=probe, check=True,
                                 capture_output=True, text=True, timeout=30).stdout.strip()
    rustc = subprocess.run(['rustc', '-Vv'], env=env, check=True, capture_output=True,
                           text=True, timeout=30).stdout
    return {'command': command, 'version_output': version, 'architecture': facts['architecture'],
            'rustc': rustc, 'offline': True, 'builder_host': facts}

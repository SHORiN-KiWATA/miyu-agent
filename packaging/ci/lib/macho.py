"""Read the few Mach-O facts a macOS release must prove, without Apple tools.

Linux 这边打包测试也要跑得动（ci.yml 的 format-and-python 在 Ubuntu 上），所以不调
otool/vtool，自己读头部与 load command。只认 64 位单架构小端文件：发布包就是这一种，
fat/universal 出现即说明构建目标错了。
"""
from pathlib import Path
import struct

MH_MAGIC_64 = 0xfeedfacf
CPU_TYPE_ARM64 = 0x0100000c
MH_EXECUTE = 0x2
LC_CODE_SIGNATURE = 0x1d
LC_BUILD_VERSION = 0x32
PLATFORM_MACOS = 1


def _version(value):
    """LC_BUILD_VERSION 把 X.Y.Z 编成 xxxx.yy.zz 三段半字节:15.0 就是 0x000f0000。"""
    major, minor, patch = value >> 16, (value >> 8) & 0xff, value & 0xff
    return f'{major}.{minor}' + (f'.{patch}' if patch else '')


def inspect(path):
    data = Path(path).read_bytes()
    if len(data) < 32:
        raise ValueError(f'Not a Mach-O executable: {path}')
    magic, cputype, _, filetype, ncmds, sizeofcmds, _, _ = struct.unpack_from('<IiiIIIII', data, 0)
    if magic != MH_MAGIC_64:
        raise ValueError(f'Not a thin 64-bit little-endian Mach-O file: {path}')
    facts = {'cputype': cputype, 'filetype': filetype, 'platform': None,
             'minos': None, 'sdk': None, 'code_signature': False}
    offset, end = 32, 32 + sizeofcmds
    if end > len(data):
        raise ValueError(f'Truncated Mach-O load commands: {path}')
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', data, offset)
        if size < 8 or offset + size > end:
            raise ValueError(f'Malformed Mach-O load command: {path}')
        if cmd == LC_BUILD_VERSION:
            platform, minos, sdk = struct.unpack_from('<III', data, offset + 8)
            facts.update(platform=platform, minos=_version(minos), sdk=_version(sdk))
        elif cmd == LC_CODE_SIGNATURE:
            facts['code_signature'] = True
        offset += size
    return facts


def require_macos_arm64_executable(path, deployment_target):
    facts = inspect(path)
    if facts['cputype'] != CPU_TYPE_ARM64 or facts['filetype'] != MH_EXECUTE:
        raise ValueError(f'Expected an arm64 Mach-O executable: {path}')
    if facts['platform'] != PLATFORM_MACOS or facts['minos'] != deployment_target:
        raise ValueError(f'Mach-O minimum macOS is {facts["minos"]}, expected {deployment_target}: {path}')
    if not facts['code_signature']:
        # Apple Silicon 拒绝执行没有签名的代码;链接器默认会做 ad-hoc 签名,缺了说明被剥掉了。
        raise ValueError(f'Mach-O executable carries no code signature: {path}')
    return facts

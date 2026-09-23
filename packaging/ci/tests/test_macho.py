"""macOS 发布二进制的三条硬事实(09-23):arm64 可执行、最低系统 = 锁文件部署目标、带签名。

Linux 的 CI 也跑这套测试,所以用手拼的最小 Mach-O,不依赖 otool。
"""
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib import macho


def build_macho(*, cputype=macho.CPU_TYPE_ARM64, filetype=macho.MH_EXECUTE, minos=(15, 0, 0),
                sdk=(15, 5, 0), signed=True, magic=macho.MH_MAGIC_64):
    def version(parts):
        return parts[0] << 16 | parts[1] << 8 | parts[2]
    commands = struct.pack('<IIIIII', macho.LC_BUILD_VERSION, 24, macho.PLATFORM_MACOS,
                           version(minos), version(sdk), 0)
    if signed:
        commands += struct.pack('<IIII', macho.LC_CODE_SIGNATURE, 16, 4096, 128)
    header = struct.pack('<IiiIIIII', magic, cputype, 0, filetype, 1 + signed, len(commands), 0, 0)
    return header + commands


class MachoTests(unittest.TestCase):
    def check(self, data):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp)/'miyu'
            path.write_bytes(data)
            return macho.require_macos_arm64_executable(path, '15.0')

    def test_release_binary_facts(self):
        facts = self.check(build_macho())
        self.assertEqual((facts['minos'], facts['sdk'], facts['code_signature']), ('15.0', '15.5', True))
        self.assertEqual(macho._version(0x000f0203), '15.2.3')

    def test_wrong_architecture_target_or_missing_signature_rejected(self):
        for data in (build_macho(cputype=0x01000007), build_macho(filetype=0x6),
                     build_macho(minos=(26, 0, 0)), build_macho(signed=False),
                     build_macho(magic=0xcafebabe), b'\x7fELF' + b'\0'*60, b'short'):
            with self.subTest(data=data[:8]), self.assertRaises(ValueError):
                self.check(data)

    def test_truncated_load_commands_rejected(self):
        data = build_macho()
        with self.assertRaises(ValueError):
            self.check(data[:40])


if __name__ == '__main__':
    unittest.main()

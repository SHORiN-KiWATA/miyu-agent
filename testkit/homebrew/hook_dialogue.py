#!/usr/bin/env python3
"""在真交互 shell（PTY）里敲一句中文，看 hook 有没有把它交给 Miyu、模型有没有回话。

用法: python3 hook_dialogue.py <沙箱目录> <zsh|bash> <转录文件>

沙箱目录下要有 home/（装好 hook 的假 HOME）、miyu/（MIYU_HOME，带 provider 配置）、
rt/（XDG_RUNTIME_DIR）。PATH 只给系统目录——miyu 不在 PATH 上，靠 hook 兜底去
/opt/homebrew/bin 找（09-23 Homebrew 验收）。兼容 macOS 自带的 python3（3.9）。
最后一行打印 PASS/FAIL；退出码 0 表示拿到了回复。
"""
import os
import pty
import re
import select
import sys
import time

sandbox, shell, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
env = {'HOME': f'{sandbox}/home', 'MIYU_HOME': f'{sandbox}/miyu', 'XDG_RUNTIME_DIR': f'{sandbox}/rt',
       'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'LANG': 'zh_CN.UTF-8',
       'SHELL': f'/bin/{shell}', 'USER': os.environ.get('USER', 'user')}
argv = ['/bin/zsh', '-i'] if shell == 'zsh' else ['/bin/bash', '-l', '-i']
LINE = '你好，请用一句话介绍你自己'


def pump(fd, seconds, chunks, idle=None):
    """读到超时，或者（已有输出后）连续 idle 秒没有新输出。"""
    deadline, last = time.time() + seconds, None
    while time.time() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.5)
        if ready:
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            chunks.append(data)
            last = time.time()
            if b'\x1b[6n' in data:  # 光标位置查询：回一个假坐标，别让程序干等
                os.write(fd, b'\x1b[24;1R')
        elif idle and last and time.time() - last > idle:
            break


pid, fd = pty.fork()
if pid == 0:
    os.execve(argv[0], argv, env)
chunks = []
pump(fd, 8, chunks, idle=2)
os.write(fd, LINE.encode() + b'\r')
started = time.time()
pump(fd, 150, chunks, idle=12)
elapsed = time.time() - started
os.write(fd, b'exit\r')
pump(fd, 5, chunks, idle=1)
try:
    os.kill(pid, 9)
except OSError:
    pass
os.waitpid(pid, 0)
raw = b''.join(chunks).decode('utf-8', 'replace')
text = re.sub(r'\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[=>()][0-9A-B]?|\r', '', raw)
with open(out_path, 'w') as out:
    out.write(text)
# 回复=敲的那句之后出现、不是转轮/思考标记/提示符/报错的一行中文。
after = text.split(LINE, 1)[-1]
replies = [line.strip() for line in after.splitlines()
           if re.search(r'[一-鿿]', line) and LINE not in line and '已思考' not in line
           and 'command not found' not in line and not line.strip().startswith(('…', '⠋'))]
ok = bool(replies)
print(f'{"PASS" if ok else "FAIL"} {shell}: {elapsed:.1f}s after typing; reply: {replies[0][:60] if ok else "-"}')
sys.exit(0 if ok else 1)

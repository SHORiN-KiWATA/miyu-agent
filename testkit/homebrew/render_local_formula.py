#!/usr/bin/env python3
"""把仓库里的 formula 渲染成指向本地 tar.gz 的版本（file:// 地址），给真 Mac 验收用。

用法（在有仓库的机器上跑，产物拷到 Mac）：

    python3 testkit/homebrew/render_local_formula.py \\
        --tarball /tmp/mh/miyu-0.6.2-1-aarch64-apple-darwin.tar.gz \\
        --sha256 <包的 sha256> --version 0.6.2 --out /tmp/miyu.rb

`--tarball` 写的是**包在 Mac 上的路径**，本机不需要有这个文件。渲染走的是发版时
`channel_update.py` 用的同一个函数，只把下载地址换成本地文件。
"""
import argparse
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT/'packaging/ci'))
from lib import homebrew  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--tarball', required=True, help='Absolute path of the tarball on the Mac.')
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--revision', type=int, default=1, help='Package revision. Default 1.')
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    if not args.tarball.startswith('/'):
        parser.error('--tarball must be absolute')
    # macOS 的 /tmp 是 /private/tmp 的链接;brew 下载时两种写法都认,这里照写。
    formula = homebrew.render_formula((ROOT/homebrew.FORMULA).read_text(), version=args.version,
        package_revision=args.revision, url='file://'+args.tarball, sha256=args.sha256)
    args.out.write_text(formula)
    print(f'{args.out}: {homebrew.formula_fields(formula)}')


if __name__ == '__main__':
    main()

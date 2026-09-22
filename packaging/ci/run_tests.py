#!/usr/bin/env python3
"""Run predefined distribution checks with isolated product data."""
import argparse
from pathlib import Path
import re
import subprocess
import sys

from lib.common import BlockedError, fresh_directory, write_json
from lib.isolation import Sandbox
from lib.process import ProcessSupervisor

SUITES = ('source-unit', 'resource-paths', 'sandbox-contract', 'installed-core',
          'embedding-live', 'renderer-live', 'daemon-lifecycle', 'macos-interaction',
          'voice-offline', 'upgrade')
FILTERS = {'resource-paths': 'distribution_resources', 'sandbox-contract': 'distribution_sandbox'}
REPO = Path(__file__).resolve().parents[2]


def execute(suite, binary, box, supervisor, report_dir, timeout, frozen):
    commands = []
    if suite not in ('source-unit', *FILTERS):
        raise BlockedError(f'Suite {suite} awaits its task implementation and required environment.')
    # `--workspace`:没有它就只跑根包,四个 crate（miyu-base / miyu-core /
    # miyu-engine / miyu-hosts）的单测一条都不跑。2026-09-23 实测 CI 上发现
    # 422 条,而本机 `refactor-check.sh` 跑的是 2667 条——差的两千多条里包括
    # 沙盒的行为测试，于是「CI 绿了」对那些代码根本不构成保证（当天差点据此
    # 宣布 macOS 沙盒验过了）。本机实测：不加 423 条，加了 825 条。
    argv = ['cargo', 'test', '--frozen' if frozen else '--locked',
            '--workspace', '--lib']
    if suite in FILTERS:
        argv.append(FILTERS[suite])
    env = box.environment()
    listing = supervisor.run(argv + ['--', '--list'], env=env, cwd=REPO,
        timeout=timeout, log=report_dir/'discovery.txt')
    commands.append(listing)
    if listing['exit_code'] != 0 or listing['timed_out']:
        return 'FAIL', 'Test discovery failed.', commands
    found = re.findall(r'^.+: test$', (report_dir/'discovery.txt').read_text(errors='replace'), re.M)
    if not found:
        return 'BLOCKED', 'No expected tests discovered. Suite cannot pass.', commands
    run = supervisor.run(argv + ['--', '--nocapture'], env=env, cwd=REPO, timeout=timeout,
                         log=report_dir/'tests.txt')
    commands.append(run)
    output = (report_dir/'tests.txt').read_text(errors='replace')
    counts = re.findall(r'test result: ok\. (\d+) passed;', output)
    passed = run['exit_code'] == 0 and not run['timed_out'] and any(int(n) > 0 for n in counts)
    return ('PASS' if passed else 'FAIL'), f'Discovered {len(found)} tests.', commands


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suite', required=True, choices=SUITES)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--report-dir', required=True, type=Path)
    parser.add_argument('--timeout', type=float, default=1800)
    parser.add_argument('--frozen', action='store_true', help='Use prepared offline Cargo inputs.')
    args = parser.parse_args()
    if args.binary is not None and not args.binary.is_absolute():
        parser.error('--binary must be an explicit absolute path')
    if args.timeout <= 0:
        parser.error('--timeout must be positive')
    try:
        report_dir = fresh_directory(args.report_dir)
    except ValueError as error:
        parser.error(str(error))
    report = {'schema_version': 1, 'suite': args.suite, 'status': 'BLOCKED', 'commands': []}
    supervisor = None
    try:
        with Sandbox() as box, ProcessSupervisor() as supervisor:
            report.update(run_id=box.run_id, sandbox_root=str(box.root))
            report['status'], report['reason'], report['commands'] = execute(
                args.suite, args.binary, box, supervisor, report_dir, args.timeout, args.frozen)
    except BlockedError as error:
        report['reason'] = str(error)
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        report.update(status='FAIL', reason=str(error))
    finally:
        if supervisor is not None:
            # A cleanup exception must not erase discovery/test exit codes or logs.
            report['commands'] = supervisor.results
    write_json(report_dir/'report.json', report)
    print(f"{report['status']}: {report.get('reason', '')} Report: {report_dir/'report.json'}")
    return {'PASS': 0, 'FAIL': 1, 'BLOCKED': 3}[report['status']]


if __name__ == '__main__':
    sys.exit(main())

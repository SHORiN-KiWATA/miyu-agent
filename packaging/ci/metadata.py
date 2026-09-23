#!/usr/bin/env python3
"""Freeze source bytes, build inputs, assets and required installation checks."""
import argparse
from pathlib import Path
import subprocess
import sys
import tomllib

from lib.common import BlockedError, canonical_json, load_json, sha256_file, write_json
from lib.manifest import COMMON, validate_manifest, validate_toolchain
from lib.matrix import PROFILES, release_matrix, selected_targets, signing_channel
from lib.source import export_source, git, source_files, snapshot_digest

REPO = Path(__file__).resolve().parents[2]
LOCKS = {'cargo': 'Cargo.lock', 'toolchain': 'packaging/common/toolchain.lock.json',
         'third_party': 'packaging/common/third-party.lock.json',
         'assets': 'packaging/common/assets.json', 'targets': 'packaging/common/targets.json'}


def create_manifest(repo, *, mode, source_ref, profile, revision, targets=None, tag=None,
                    include_new=(), declaration=None):
    repo = Path(repo).resolve()
    selected = selected_targets(profile, targets)
    source = git(repo, 'rev-parse', '--verify', source_ref+'^{commit}')
    if source != git(repo, 'rev-parse', 'HEAD'):
        raise ValueError('Check out the requested source commit before exporting its snapshot.')
    package = tomllib.loads((repo/'Cargo.toml').read_text())['package']
    cargo_lock = tomllib.loads((repo/'Cargo.lock').read_text())
    root = [p for p in cargo_lock['package'] if p['name'] == package['name'] and 'source' not in p]
    if len(root) != 1 or root[0]['version'] != package['version']:
        raise ValueError('Cargo.toml and Cargo.lock application versions differ.')
    dirty = bool(git(repo, 'status', '--porcelain', '--untracked-files=all'))
    if mode == 'release' and (dirty or include_new):
        raise ValueError('Formal releases require a clean committed source tree.')
    for relative in LOCKS.values():
        if not (repo/relative).is_file():
            raise BlockedError(f'Required input lock is not implemented: {relative}')
    toolchain = load_json(repo/LOCKS['toolchain'])
    validate_toolchain(toolchain)
    third_party = load_json(repo/LOCKS['third_party'])
    if third_party.get('schema_version') != 1:
        raise ValueError('Unsupported third-party lock schema.')
    records = source_files(repo, include_new)
    exported = {record['path'] for record in records}
    if set(LOCKS.values()) - exported:
        raise ValueError('Explicitly include all new input lock files in the source snapshot.')
    tag_commit = git(repo, 'rev-parse', '--verify', f'refs/tags/{tag}^{{commit}}') if tag else None
    builds, assets, checks = release_matrix(repo/LOCKS['targets'], profile, selected,
        package['version'], revision, toolchain['fedora']['version'])
    manifest = {'schema_version': 1, 'mode': mode, 'profile': profile,
        'version': package['version'], 'package_revision': revision, 'tag': tag,
        'tag_commit': tag_commit, 'source_commit': source, 'source_dirty': dirty,
        'source_snapshot_sha256': snapshot_digest(records),
        'source_date_epoch': int(git(repo, 'show', '-s', '--format=%ct', source)),
        'wiki_commit': third_party['wiki']['commit'],
        'locks': {name: sha256_file(repo/path) for name, path in LOCKS.items()},
        'toolchain': toolchain, 'builders': toolchain['builders'],
        'fedora_version': toolchain['fedora']['version'], 'targets': selected,
        'builds': builds, 'assets': assets, 'checks': checks, 'workflow_commit': source,
        'release_declaration': declaration,
        'channels': {'github': 'prerelease' if profile == 'preview-core' else 'stable',
            'macos_direct_signing': signing_channel(profile)}}
    return validate_manifest(manifest, repo/LOCKS['targets']), records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', required=True, choices=('preview', 'release'))
    parser.add_argument('--source-ref', required=True)
    parser.add_argument('--profile', required=True, choices=PROFILES)
    parser.add_argument('--revision', required=True, type=int)
    parser.add_argument('--targets', help='Explicit comma-separated preview installation targets.')
    parser.add_argument('--tag')
    parser.add_argument('--declaration', type=Path, help='Controlled tag/source exception JSON.')
    parser.add_argument('--include-new', action='append', default=[], metavar='RELATIVE_FILE',
                        help='Explicit untracked source file to export. Repeat for each file.')
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--github-output', type=Path)
    args = parser.parse_args()
    if args.out.exists() or args.out.is_symlink():
        parser.error('Output manifest already exists.')
    if args.revision < 1:
        parser.error('--revision must be positive')
    try:
        manifest, records = create_manifest(REPO, mode=args.mode, source_ref=args.source_ref,
            profile=args.profile, revision=args.revision,
            targets=args.targets.split(',') if args.targets is not None else None, tag=args.tag,
            include_new=args.include_new,
            declaration=load_json(args.declaration) if args.declaration else None)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        export_source(REPO, args.out.parent/'source', records)
        write_json(args.out, manifest)
        if args.github_output:
            import json
            matrix = {'include': [{'build_id': key} for key in manifest['builds']]}
            with args.github_output.open('a', encoding='utf-8') as output:
                output.write('matrix='+json.dumps(matrix, separators=(',', ':'))+'\n')
                output.write('tag='+str(manifest['tag'] or '')+'\nsource_sha='+manifest['source_commit']+'\n')
        print(f'Frozen {len(records)} source entries, {len(manifest["assets"])} assets, '
              f'{len(manifest["targets"])} installation targets: {args.out}')
        return 0
    except BlockedError as error:
        print(f'BLOCKED: {error}', file=sys.stderr)
        return 3
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as error:
        print(f'ERROR: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())

"""Validate release inputs including their frozen, complete check matrix."""
from pathlib import Path
import re

from .common import load_json
from .matrix import SMOKE_PROFILES, release_matrix, selected_targets, signing_channel

COMMON = Path(__file__).resolve().parents[2] / 'common'
FIELDS = {'schema_version', 'mode', 'profile', 'version', 'package_revision', 'tag',
          'tag_commit', 'source_commit', 'source_snapshot_sha256', 'source_dirty',
          'source_date_epoch', 'wiki_commit', 'locks', 'builders', 'toolchain',
          'fedora_version', 'targets', 'builds', 'assets', 'checks', 'workflow_commit',
          'release_declaration', 'channels'}
LOCK_NAMES = ('cargo', 'toolchain', 'third_party', 'assets', 'targets')


def digest(value, size=64):
    if (not isinstance(value, str) or not re.fullmatch(f'[0-9a-f]{{{size}}}', value)
            or len(set(value)) == 1):
        raise ValueError(f'Expected a non-placeholder {size}-digit digest.')


def validate_toolchain(lock):
    if lock.get('schema_version') != 1:
        raise ValueError('Unsupported toolchain schema version.')
    for field in ('rust', 'msrv', 'python_minimum'):
        if not re.fullmatch(r'\d+\.\d+(?:\.\d+)?', lock.get(field, '')):
            raise ValueError(f'Invalid toolchain version: {field}')
    if tuple(map(int, lock['python_minimum'].split('.'))) < (3, 11):
        raise ValueError('Python must be at least 3.11.')
    nfpm = lock['nfpm']
    digest(nfpm['sha256'])
    if not nfpm['url'].startswith('https://'):
        raise ValueError('nFPM URL must use HTTPS.')
    for name in ('gnu-x86_64', 'arch-x86_64'):
        builder = lock['builders'][name]
        if builder['architecture'] != 'x86_64' or '@sha256:' not in builder['image']:
            raise ValueError(f'Builder must pin an x86_64 image digest: {name}')
        digest(builder['image'].split('@sha256:')[-1])
    mac = lock['builders']['macos-arm64']
    if (mac['architecture'] != 'arm64' or mac['deployment_target'] != '15.0'
            or mac['minimum_test_os'] != '15.0' or not mac['sdk'].startswith('macosx')):
        raise ValueError('Mac builder violates the arm64/macOS 15 contract.')
    fedora = lock['fedora']['version']
    if type(fedora) is not int or fedora <= 0:
        raise ValueError('Fedora must be a resolved stable version number.')
    digest(lock['fedora']['source_sha256'])


def validate_manifest(manifest, catalog=COMMON/'targets.json'):
    if set(manifest) != FIELDS:
        raise ValueError(f'Unexpected/missing release fields: {sorted(set(manifest) ^ FIELDS)}')
    if type(manifest['schema_version']) is not int or manifest['schema_version'] != 1:
        raise ValueError('Unsupported release schema version.')
    if manifest['mode'] not in ('preview', 'release'):
        raise ValueError('Unknown release mode.')
    if not re.fullmatch(r'\d+\.\d+\.\d+', manifest['version']):
        raise ValueError('Version must have three numeric components.')
    for field in ('package_revision', 'source_date_epoch', 'fedora_version'):
        if type(manifest[field]) is not int or manifest[field] < 1:
            raise ValueError(f'{field} must be a positive integer.')
    for field in ('source_commit', 'wiki_commit', 'workflow_commit'):
        digest(manifest[field], 40)
    digest(manifest['source_snapshot_sha256'])
    if type(manifest['source_dirty']) is not bool:
        raise ValueError('source_dirty must be a boolean.')
    if set(manifest['locks']) != set(LOCK_NAMES):
        raise ValueError('Release input locks are incomplete.')
    for value in manifest['locks'].values():
        digest(value)
    validate_toolchain(manifest['toolchain'])
    if manifest['builders'] != manifest['toolchain']['builders']:
        raise ValueError('Builders differ from the frozen toolchain lock.')
    if manifest['fedora_version'] != manifest['toolchain']['fedora']['version']:
        raise ValueError('Fedora version differs from the frozen lock.')
    selection = manifest['targets'] if manifest['profile'] == 'preview-core' else None
    selected = selected_targets(manifest['profile'], selection)
    if manifest['targets'] != selected:
        raise ValueError('Required installation target matrix is incomplete or unordered.')
    builds, assets, checks = release_matrix(catalog, manifest['profile'], selected,
        manifest['version'], manifest['package_revision'], manifest['fedora_version'])
    for field, expected in (('builds', builds), ('assets', assets), ('checks', checks)):
        if manifest[field] != expected:
            raise ValueError(f'Release {field} differ from the fixed profile contract.')
    if len({asset['filename'] for asset in assets}) != len(assets):
        raise ValueError('Duplicate asset filenames.')
    channels = {'github': 'prerelease' if manifest['profile'] == 'preview-core' else 'stable',
                'macos_direct_signing': signing_channel(manifest['profile'])}
    if manifest['channels'] != channels:
        raise ValueError('Channel signing policy differs from the profile.')
    if manifest['mode'] == 'release':
        if manifest['source_dirty'] or manifest['tag'] != 'v'+manifest['version']:
            raise ValueError('Release requires clean source and matching version tag.')
        digest(manifest['tag_commit'], 40)
        if manifest['tag_commit'] != manifest['source_commit']:
            declaration = manifest['release_declaration']
            if (not isinstance(declaration, dict) or declaration.get('source_commit') != manifest['source_commit']
                    or declaration.get('tag_commit') != manifest['tag_commit']
                    or not declaration.get('reason', '').strip()):
                raise ValueError('Tag/source mismatch requires an explicit controlled declaration.')
    elif (manifest['profile'] not in ('preview-core', *SMOKE_PROFILES) or manifest['tag'] is not None
            or manifest['tag_commit'] is not None or manifest['release_declaration'] is not None):
        raise ValueError('Local preview requires preview-core and no release tag/declaration.')
    return manifest


def read_manifest(path):
    return validate_manifest(load_json(path))

#!/usr/bin/env python3
"""Stage declared binaries and complete package resources without executing Miyu."""
import argparse
from pathlib import Path
import re
import sys

from lib.common import fresh_directory, load_json, sha256_file, write_json
from lib.identity import build_identity
from lib.inputs import verify_prepared
from lib.native_build import NATIVE_VENDOR, cargo_command, is_native
from prepare import verify_source
from lib.staging import install_file, selected_files, tree_manifest, validate_assets


BUILD_EVIDENCE_FIELDS = ('build_id', 'component', 'build_identity', 'binary_sha256',
    'builder_image', 'source_commit', 'source_snapshot_sha256', 'release_input_sha256',
    'rustc', 'offline', 'command')
# macOS 原生构建没有镜像 digest,构建环境的身份是宿主系统 / Xcode / SDK(09-23)。
# 字段集合定死:宿主记录会进公开的 provenance,多一个字段就可能多带出一条本机路径。
HOST_FIELDS = {'kind', 'architecture', 'system', 'system_build', 'xcode', 'sdk', 'clang',
               'deployment_target', 'runner_image'}


def builder_field(manifest, build_id):
    return 'builder_host' if is_native(manifest['builds'][build_id]) else 'builder_image'


def validate_builder(value, manifest, build_id):
    if builder_field(manifest, build_id) == 'builder_image':
        if not isinstance(value, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', value):
            raise ValueError('Build evidence requires immutable image and binary SHA256 digests.')
        return
    lock = manifest['builders'][build_id]
    if (not isinstance(value, dict) or set(value) != HOST_FIELDS
            or not all(isinstance(item, str) for item in value.values())
            or value['kind'] != 'macos-native' or value['architecture'] != lock['architecture']
            or value['xcode'] != lock['xcode'] or value['sdk'] != lock['sdk']
            or value['deployment_target'] != lock['deployment_target']
            or not all(value[key] for key in ('system', 'system_build', 'clang'))):
        raise ValueError(f'Native build evidence differs from the locked macOS builder: {build_id}')


def validate_build_evidence(record, manifest, input_hash, build_id, component, binary_hash):
    """Validate and select portable evidence. Host Docker mounts never enter release assets."""
    fields = tuple(builder_field(manifest, build_id) if key == 'builder_image' else key
                   for key in BUILD_EVIDENCE_FIELDS)
    if not isinstance(record, dict) or any(key not in record for key in fields):
        raise ValueError('Required build evidence is missing.')
    evidence = {key: record[key] for key in fields}
    expected = {'build_id': build_id, 'component': component,
        'build_identity': build_identity(manifest, build_id, component),
        'binary_sha256': binary_hash, 'source_commit': manifest['source_commit'],
        'source_snapshot_sha256': manifest['source_snapshot_sha256'],
        'release_input_sha256': input_hash}
    if any(evidence[key] != value for key, value in expected.items()):
        raise ValueError(f'Build evidence differs from frozen input or binary: {build_id}/{component}')
    validate_builder(evidence[builder_field(manifest, build_id)], manifest, build_id)
    if not isinstance(binary_hash, str) or not re.fullmatch(r'[0-9a-f]{64}', binary_hash):
        raise ValueError('Build evidence requires immutable image and binary SHA256 digests.')
    build = manifest['builds'][build_id]
    command = cargo_command(build['target'], component, build['features'][component],
                            NATIVE_VENDOR if is_native(build) else '/inputs/vendor')
    if (evidence['offline'] is not True or evidence['command'] != command
            or not isinstance(evidence['rustc'], str)
            or not evidence['rustc'].startswith('rustc '+manifest['toolchain']['rust']+' ')):
        raise ValueError('Build evidence differs from the frozen compiler or offline command.')
    return evidence


def payload_binary_hash(inventory, component):
    binary = 'bin/'+('miyu-voice' if component == 'voice' else 'miyu')
    entries = [entry for entry in inventory if entry['path'] == binary]
    if len(entries) != 1 or entries[0]['type'] != 'file' or entries[0].get('size', 0) <= 0:
        raise ValueError('Package binary payload is missing or duplicated.')
    return entries[0].get('sha256')


def stage(manifest_path, inputs, build_id, build_root, destination):
    manifest, source = verify_source(manifest_path)
    verify_prepared(inputs,manifest,manifest_path)
    if build_id not in manifest['builds']:
        raise ValueError('Build ID is not declared in the release input.')
    catalog_path = source/'packaging/common/assets.json'
    if sha256_file(catalog_path) != manifest['locks']['assets']:
        raise ValueError('Resource catalog differs from the frozen input.')
    catalog = validate_assets(load_json(catalog_path))
    destination = fresh_directory(destination)
    roots = {'source': source, 'wiki': inputs/'wiki', 'runtime': inputs/'runtime'}
    components = manifest['builds'][build_id]['components']
    evidence = {}
    for component in components:
        record = load_json(build_root/component/'build-record.json')
        binary = build_root/component/('miyu-voice' if component == 'voice' else 'miyu')
        evidence[component] = validate_build_evidence(record, manifest, sha256_file(manifest_path),
            build_id, component, sha256_file(binary))
        install_file(binary, destination/component/'bin'/binary.name, 0o755)
        if component == 'core':
            (destination/component/'bin/miyupm').symlink_to('miyu')
    for rule in catalog['assets']:
        if rule['component'] not in components or build_id not in rule.get('build_ids', [build_id]):
            continue
        for source_file, relative in selected_files(rule, roots):
            install_file(source_file, destination/rule['component']/relative, int(rule['mode'], 8))
    result = {'schema_version': 1, 'build_id': build_id,
        'release_input_sha256': sha256_file(manifest_path),
        'source_snapshot_sha256': manifest['source_snapshot_sha256'],
        'build_evidence': evidence,
        'components': {component: tree_manifest(destination/component) for component in components}}
    write_json(destination/'stage-manifest.json', result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--inputs', required=True, type=Path)
    parser.add_argument('--build-id', required=True)
    parser.add_argument('--build-root', required=True, type=Path)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    try:
        result = stage(args.manifest, args.inputs, args.build_id, args.build_root, args.out)
        print(f'Staged {args.build_id}: '+', '.join(result['components']))
        return 0
    except (ValueError, KeyError, OSError) as error:
        print(f'ERROR: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())

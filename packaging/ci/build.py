#!/usr/bin/env python3
"""Build declared components offline: pinned Linux containers, or the locked Xcode on macOS."""
import argparse
import os
from pathlib import Path
import subprocess
import sys

from lib.common import BlockedError, fresh_directory, load_json, sha256_file, write_json
from lib.identity import build_identity
from lib.manifest import read_manifest
from lib.inputs import verify_prepared
from lib.native_build import compile_native, is_native
from prepare import verify_source


def build(args):
    manifest, source = verify_source(args.manifest)
    config = manifest['builds'].get(args.build_id)
    if config is None or args.component not in config['components']:
        raise ValueError('Build/component is not declared in the frozen input.')
    inputs = args.inputs.resolve()
    prepared = verify_prepared(inputs,manifest,args.manifest,require_vendor=True)
    if not prepared.get('vendor_complete'):
        raise BlockedError('Cargo vendor inputs are incomplete. Run prepare without --skip-vendor.')
    if prepared['source_snapshot_sha256'] != manifest['source_snapshot_sha256']:
        raise ValueError('Prepared inputs reference a different source snapshot.')
    if is_native(config):
        return record_build(args, manifest, *build_native(args, manifest, source, inputs))
    if not args.builder_image:
        raise BlockedError('Provide the prepared native builder image with --builder-image.')
    image = subprocess.run(['docker','image','inspect',args.builder_image,'--format','{{.Id}}'],
        check=True,capture_output=True,text=True,timeout=30).stdout.strip()
    out = fresh_directory(args.out)
    target = (args.target_cache or out/'target').resolve()
    target.mkdir(parents=True,exist_ok=True)
    identity = build_identity(manifest,args.build_id,args.component)
    name = 'miyu-build-'+identity[:12]+'-'+args.component
    command = ['docker','run','--rm','--name',name,'--network','none',
        '--label','io.miyu.distribution.owner=distribution-2026-09-14',
        '--user',f'{os.getuid()}:{os.getgid()}',
        '--mount',f'type=bind,src={source},dst=/source,readonly',
        '--mount',f'type=bind,src={inputs},dst=/inputs,readonly',
        '--mount',f'type=bind,src={out},dst=/build',
        '--mount',f'type=bind,src={target},dst=/target',image,
        'python3','/source/packaging/ci/lib/container_build.py',
        '--component',args.component,'--target',config['target'],
        '--build-identity',identity,'--epoch',str(manifest['source_date_epoch'])]
    try:
        with (out/'build.log').open('wb') as log:
            subprocess.run(command,check=True,stdout=log,stderr=subprocess.STDOUT,timeout=7500)
    finally:
        # Explicitly stop this exact owned container on timeout/interruption as well.
        subprocess.run(['docker','rm','-f',name],stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL,timeout=30)
    return record_build(args, manifest, out, load_json(out/'compile.json'),
                        builder_image=image, container_command=command)


def build_native(args, manifest, source, inputs):
    """macOS 没有容器:同一份冻结输入,在锁定的 Xcode 上原地编(lib/native_build.py)。"""
    out = fresh_directory(args.out)
    target = (args.target_cache or out/'target').resolve()
    target.mkdir(parents=True,exist_ok=True)
    identity = build_identity(manifest,args.build_id,args.component)
    return out, compile_native(manifest,args.build_id,args.component,source,inputs,out,target,identity)


def record_build(args, manifest, out, compiled, **builder):
    binary = out/('miyu-voice' if args.component == 'voice' else 'miyu')
    if compiled['version_output'] != f'{binary.name} {manifest["version"]}':
        raise ValueError('Built binary reports an unexpected application version.')
    record = dict(compiled, schema_version=1, build_id=args.build_id, component=args.component,
        build_identity=build_identity(manifest,args.build_id,args.component),
        binary_sha256=sha256_file(binary), source_commit=manifest['source_commit'],
        source_snapshot_sha256=manifest['source_snapshot_sha256'],
        release_input_sha256=sha256_file(args.manifest), **builder)
    write_json(out/'build-record.json',record)
    return record


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest',required=True,type=Path)
    parser.add_argument('--inputs',required=True,type=Path)
    parser.add_argument('--build-id',required=True)
    parser.add_argument('--component',required=True,choices=('core','voice'))
    parser.add_argument('--out',required=True,type=Path)
    parser.add_argument('--builder-image',help='Prepared Linux image, resolved to an immutable local ID. Not used by macOS.')
    parser.add_argument('--target-cache',type=Path,help='Explicit reusable Cargo cache for this build/component only.')
    args=parser.parse_args()
    try:
        result=build(args)
        print(f'Built {args.build_id}/{args.component}: {result["binary_sha256"]}')
        return 0
    except BlockedError as error:
        print(f'BLOCKED: {error}',file=sys.stderr)
        return 3
    except (ValueError,KeyError,OSError,subprocess.SubprocessError) as error:
        print(f'ERROR: {error}',file=sys.stderr)
        return 1


if __name__=='__main__':
    sys.exit(main())

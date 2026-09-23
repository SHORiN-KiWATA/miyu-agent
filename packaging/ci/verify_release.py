#!/usr/bin/env python3
"""Aggregate all frozen checks and create a minimal, verified publish directory."""
import argparse
import hashlib
from pathlib import Path
import shutil
import sys

from lib.common import canonical_json, fresh_directory, load_json, sha256_file, write_json
from lib.native_verify import version_tuple
from lib.release_bundle import bundle_checksums, output_name, sums_name, verify_bundle
from prepare import verify_source
from stage import payload_binary_hash, validate_build_evidence

# macOS 验收报告公开哪些宿主信息:系统版本够不够低、是不是 arm64,不带机器名与路径。
PUBLIC_HOST_FIELDS = ('kind', 'architecture', 'system', 'system_build', 'runner_image')


def public_report(report, records, manifest, cleanup_path):
    """Publish selected successful probe fields, without commands, paths or credentials."""
    target = report['target']
    images = manifest['toolchain']['install_images']
    if target in images:
        if report.get('image') != images[target]:
            raise ValueError('Installation report image differs from the frozen input.')
    else:
        validate_native_host(report, manifest)
    selected = {key: report[key] for key in ('target', 'status', 'image', 'source_commit',
        'source_snapshot_sha256', 'release_input_sha256')}
    if target not in images:
        selected['host'] = {key: report['host'][key] for key in PUBLIC_HOST_FIELDS}
    selected['checks'] = [{key: check[key] for key in
        ('asset_id', 'target', 'check', 'status', 'artifact_sha256')} for check in report['checks']]
    results = report.get('results', {})
    expected = {check['asset_id'] for check in report['checks']}
    if set(results) != expected:
        raise ValueError('Installed binary probe coverage differs from checked assets.')
    selected['results'] = {}
    for asset_id in sorted(expected):
        result = results[asset_id]
        record = records[asset_id]
        component = record['asset']['component']
        binary = 'miyu-voice' if component == 'voice' else 'miyu'
        if (result.get('asset_id') != asset_id
                or result.get('binary_sha256') != record['build_evidence']['binary_sha256']
                or result.get('version') != binary+' '+manifest['version']):
            raise ValueError('Installed binary probe differs from build evidence.')
        public = {key: result[key] for key in ('asset_id', 'binary_sha256', 'version', 'files_verified')}
        if component == 'core':
            live = result.get('provider_live', {})
            if (live.get('type') != 'done' or not isinstance(live.get('text'), str)
                    or not live['text'].strip() or live.get('provider_id') != 'opencodego'
                    or live.get('model') != 'deepseek-v4.1-flash'):
                raise ValueError('Real provider evidence is missing or failed.')
            public['provider_live'] = {key: live[key] for key in ('type', 'text', 'provider_id', 'model')}
            if isinstance(live.get('elapsed_ms'), (int, float)):
                public['provider_live']['elapsed_ms'] = live['elapsed_ms']
            if isinstance(live.get('usage'), dict):
                public['provider_live']['usage'] = {key: value for key, value in live['usage'].items()
                    if key in ('input_tokens', 'output_tokens', 'total_tokens', 'cache_read_tokens',
                               'cache_write_tokens') and isinstance(value, (int, float))}
        selected['results'][asset_id] = public
    if cleanup_path.exists():
        cleanup = load_json(cleanup_path)
        if cleanup.get('kind') == 'native':
            # macOS 没有容器可删;要确认的是验收用的 Homebrew tap 已经卸掉。
            if cleanup.get('remaining') != '':
                raise ValueError('Installation environment cleanup is not confirmed.')
            selected['cleanup'] = {key: cleanup[key] for key in ('kind', 'remaining')}
            return selected
        if (cleanup.get('remove_exit_code') != 0 or cleanup.get('list_exit_code') != 0
                or cleanup.get('remaining') != ''):
            raise ValueError('Installation environment cleanup is not confirmed.')
        selected['cleanup'] = {key: cleanup[key] for key in
            ('remove_exit_code', 'list_exit_code', 'remaining')}
    return selected


def validate_native_host(report, manifest):
    host = report.get('host')
    minimum = manifest['builders'][report['target']]['minimum_test_os']
    if (report.get('image') is not None or not isinstance(host, dict)
            or any(not isinstance(host.get(key), str) for key in PUBLIC_HOST_FIELDS)
            or host['kind'] != 'macos-native' or host['architecture'] != 'arm64'
            or version_tuple(host['system']) < version_tuple(minimum)):
        raise ValueError('Native installation report host differs from the frozen macOS contract.')


def aggregate(manifest_path,artifacts,reports,publish_dir):
    manifest,source=verify_source(manifest_path)
    input_hash=sha256_file(manifest_path)
    assets={asset['id']:asset for asset in manifest['assets']}
    records={}
    builds={}
    for asset_id,asset in assets.items():
        folder=artifacts/asset_id
        record=load_json(folder/'package-record.json')
        if (record['asset']!=asset or record['release_input_sha256']!=input_hash
                or record['source_snapshot_sha256']!=manifest['source_snapshot_sha256']
                or record['source_commit']!=manifest['source_commit']
                or sha256_file(folder/asset['filename'])!=record['sha256']):
            raise ValueError(f'Final package identity/hash mismatch: {asset_id}')
        files=record['files']
        license_root='share/licenses/miyu-voice/' if asset['component']=='voice' else 'share/licenses/miyu/'
        if not any(f['path']==license_root+'LICENSE' and f['size']>0 for f in files):
            raise ValueError(f'Package license is missing: {asset_id}')
        evidence=validate_build_evidence(record.get('build_evidence'),manifest,input_hash,
            asset['build_id'],asset['component'],payload_binary_hash(files,asset['component']))
        key=(asset['build_id'],asset['component'])
        if key in builds and builds[key]!=evidence:
            raise ValueError(f'Conflicting evidence for shared build: {key}')
        builds[key]=evidence
        records[asset_id]=record
    found={}
    acceptance_reports=[]
    for target in manifest['targets']:
        report=load_json(reports/target/'report.json')
        if (report['target']!=target or report['status']!='PASS'
                or report['release_input_sha256']!=input_hash
                or report['source_commit']!=manifest['source_commit']
                or report['source_snapshot_sha256']!=manifest['source_snapshot_sha256']):
            raise ValueError(f'Missing, failed or stale installation report: {target}')
        for check in report['checks']:
            key=(check['asset_id'],target,check['check'])
            if key in found or check['target']!=target or check['asset_id'] not in records:
                raise ValueError('Duplicate or undeclared check report.')
            if check['status']!='PASS' or check['artifact_sha256']!=records[check['asset_id']]['sha256']:
                raise ValueError(f'Required check did not pass against final bytes: {key}')
            found[key]=check
        acceptance_reports.append(public_report(report,records,manifest,reports/target/'cleanup.json'))
    expected={(c['asset_id'],c['target'],c['check']) for c in manifest['checks'] if c['required']}
    if set(found)!=expected:
        raise ValueError(f'Required check coverage differs: missing={sorted(expected-set(found))}')
    out=fresh_directory(publish_dir)
    files=[]
    def register(path,kind,**extra):
        files.append(dict(filename=path.name,sha256=sha256_file(path),size=path.stat().st_size,kind=kind,**extra))
    for asset_id,record in records.items():
        asset=assets[asset_id]
        path=out/asset['filename']
        shutil.copyfile(artifacts/asset_id/path.name,path)
        register(path,'package',asset_id=asset_id,build_id=asset['build_id'],component=asset['component'])
    version=f'{manifest["version"]}-{manifest["package_revision"]}'
    input_copy=out/f'release-input-{version}.json'
    shutil.copyfile(manifest_path,input_copy)
    register(input_copy,'input')
    acceptance=out/f'acceptance-{version}.json'
    write_json(acceptance,{'schema_version':1,'version':manifest['version'],
        'package_revision':manifest['package_revision'],'profile':manifest['profile'],
        'release_input_sha256':input_hash,'reports':acceptance_reports})
    register(acceptance,'acceptance')
    provenance=out/f'provenance-{version}.json'
    write_json(provenance,{'_type':'https://in-toto.io/Statement/v1',
        'subject':[{'name':r['filename'],'digest':{'sha256':r['sha256']}} for r in files if r['kind']=='package'],
        'predicateType':'https://slsa.dev/provenance/v1','predicate':{
            'buildDefinition':{'buildType':'https://github.com/SHORiN-KiWATA/miyu-agent/distribution/v1',
                'externalParameters':manifest,'internalParameters':{},
                'resolvedDependencies':[{'uri':'git+https://github.com/SHORiN-KiWATA/miyu-agent',
                    'digest':{'gitCommit':manifest['source_commit'],'sourceSnapshotSha256':manifest['source_snapshot_sha256']}}]+
                    [{'uri':'docker-image:'+image,'digest':{'sha256':image.removeprefix('sha256:')}}
                     for image in sorted({build['builder_image'] for build in builds.values()
                                          if 'builder_image' in build})]+
                    [{'uri':'macos-host:'+host['sdk'],'digest':{'sha256':hashlib.sha256(canonical_json(host)).hexdigest()}}
                     for host in [build['builder_host'] for key,build in sorted(builds.items())
                                  if 'builder_host' in build]]},
            'runDetails':{'builder':{'id':'miyu-distribution-local'},'metadata':{'invocationId':input_hash},
                'byproducts':[builds[key] for key in sorted(builds)]}}})
    register(provenance,'provenance')
    # An SPDX file inventory records the exact distributed payload, including licenses.
    for build_id,build in manifest['builds'].items():
        for component in build['components']:
            record=next(r for r in records.values() if r['asset']['build_id']==build_id and r['asset']['component']==component)
            sbom=out/f'sbom-{version}-{build_id}-{component}.spdx.json'
            import datetime
            created=datetime.datetime.fromtimestamp(manifest['source_date_epoch'],datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
            payload=[f for f in record['files'] if f['type']=='file']
            spdx_files=[{'SPDXID':f'SPDXRef-File-{i}','fileName':'./'+f['path'],
                'checksums':[{'algorithm':'SHA256','checksumValue':f['sha256']}],
                'licenseConcluded':'NOASSERTION','copyrightText':'NOASSERTION'} for i,f in enumerate(payload)]
            write_json(sbom,{'spdxVersion':'SPDX-2.3','dataLicense':'CC0-1.0','SPDXID':'SPDXRef-DOCUMENT',
                'name':f'miyu-{build_id}-{component}-{version}',
                'documentNamespace':f'https://miyu.dev/spdx/{input_hash}/{build_id}/{component}',
                'creationInfo':{'creators':['Tool: Miyu-distribution'],'created':created},
                'files':spdx_files,'relationships':[{'spdxElementId':'SPDXRef-DOCUMENT',
                    'relationshipType':'DESCRIBES','relatedSpdxElement':f['SPDXID']} for f in spdx_files]})
            register(sbom,'sbom',build_id=build_id,component=component)
    screenshots=source/'docs/releases'/manifest['version']/'oobe'
    if screenshots.exists():
        for screenshot in sorted(screenshots.glob('*.png')):
            if not screenshot.read_bytes().startswith(b'\x89PNG\r\n\x1a\n'):
                raise ValueError('Release screenshot is not a PNG.')
            destination=out/screenshot.name
            shutil.copyfile(screenshot,destination)
            register(destination,'screenshot')
    output={'schema_version':1,'version':manifest['version'],'package_revision':manifest['package_revision'],
        'source_commit':manifest['source_commit'],'source_snapshot_sha256':manifest['source_snapshot_sha256'],
        'profile':manifest['profile'],'release_input_sha256':input_hash,'files':files,
        'checks':[found[k] for k in sorted(found)]}
    write_json(out/output_name(manifest),output)
    names=[r['filename'] for r in files]+[output_name(manifest)]
    (out/sums_name(manifest)).write_text(bundle_checksums(out,names))
    verify_bundle(manifest,out)
    return output


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest',required=True,type=Path)
    parser.add_argument('--artifacts',required=True,type=Path)
    parser.add_argument('--reports',required=True,type=Path)
    parser.add_argument('--publish-dir',required=True,type=Path)
    args=parser.parse_args()
    try:
        output=aggregate(args.manifest,args.artifacts,args.reports,args.publish_dir)
        print(f'PASS: {len(output["checks"])} required checks. Publish allowlist: {args.publish_dir}')
        return 0
    except (ValueError,KeyError,OSError) as error:
        print(f'ERROR: {error}',file=sys.stderr)
        return 1


if __name__=='__main__':
    sys.exit(main())

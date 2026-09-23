#!/usr/bin/env python3
"""Install candidate packages in clean containers (Linux) or a relocated prefix plus Homebrew
(macOS), and run the authorized provider smoke."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

from lib.common import BlockedError, fresh_directory, load_json, sha256_file, write_json
from lib.isolation import Sandbox
from lib.manifest import read_manifest
from lib.matrix import SMOKE_PROFILES
from lib.native_verify import verify_macos
from prepare import verify_source

NATIVE_TARGETS = ('macos-arm64',)


def local_provider(config_path):
    if config_path is None:
        raise BlockedError('Real provider acceptance requires an explicit --provider-config path.')
    config=load_json(config_path)
    providers=[p for p in config.get('providers',[]) if p.get('id')=='opencodego']
    if len(providers)!=1 or 'deepseek-v4.1-flash' not in providers[0].get('models',[]):
        raise BlockedError('The requested opencodego/deepseek-v4.1-flash provider is missing.')
    if not providers[0].get('api_key'):
        raise BlockedError('The requested provider has no configured credential.')
    return providers[0]


def install_commands(target, package_paths):
    if target.startswith(('debian','ubuntu','mint')):
        commands=[]
        commands += [['apt-get','update'],['apt-get','install','-y','python3','ca-certificates'],
                     ['apt-get','install','-y',*package_paths]]
        return commands
    if target=='fedora-current-x86_64':
        return [['dnf','install','-y','python3'],['dnf','install','-y',*package_paths]]
    if target=='arch-x86_64':
        return [['sh','-c',"printf '%s\\n' 'Server = https://archive.archlinux.org/repos/2026/09/13/$repo/os/$arch' > /etc/pacman.d/mirrorlist"],
                ['pacman','-Syuu','--noconfirm','--disable-download-timeout','python'],
                ['pacman','-U','--noconfirm','--disable-download-timeout',*package_paths]]
    raise ValueError('Unsupported container installation target.')


def cleanup_container(name, box, out, secret):
    """Confirm Docker no longer owns the bind mount before deleting its home."""
    removal=subprocess.run(['docker','rm','-f',name],capture_output=True,text=True,timeout=30)
    listing=subprocess.run(['docker','container','ls','-a','--filter',f'name=^{name}$',
                            '--format','{{.Names}}'],capture_output=True,text=True,timeout=30)
    write_json(out/'cleanup.json',{'container':name,'remove_exit_code':removal.returncode,
        'list_exit_code':listing.returncode,'remaining':listing.stdout.strip(),
        'stderr':(removal.stderr+listing.stderr).replace(secret,'[REDACTED]')})
    if listing.returncode or listing.stdout.strip():
        raise ValueError(f'Container cleanup could not be confirmed. Retained test home: {box.root}')
    box.cleanup()
    if removal.returncode:
        raise ValueError('Container removal failed even though a subsequent listing confirmed absence.')


def verify(args):
    manifest=read_manifest(args.manifest)
    if manifest['profile'] not in SMOKE_PROFILES:
        raise BlockedError('This executor implements the explicitly amended smoke profiles only.')
    if args.target_id not in manifest['targets']:
        raise ValueError('Installation target is not declared in the release input.')
    native=args.target_id in NATIVE_TARGETS
    if not native and (args.skip_provider or args.allow_homebrew_changes):
        raise ValueError('--skip-provider and --allow-homebrew-changes only apply to macOS.')
    # 预览构建可以不给凭据(provider-live 记 SKIPPED,报告不会是 PASS);正式发版照样要。
    provider=None if native and args.skip_provider else local_provider(args.provider_config)
    out=fresh_directory(args.report_dir)
    required=[c for c in manifest['checks'] if c['target']==args.target_id]
    asset_ids={c['asset_id'] for c in required}
    records={}
    for asset in manifest['assets']:
        if asset['id'] not in asset_ids:
            continue
        directory=args.packages/asset['id']
        record=load_json(directory/'package-record.json')
        if (record['asset']!=asset or record['release_input_sha256']!=sha256_file(args.manifest)
                or record['sha256']!=sha256_file(directory/asset['filename'])):
            raise ValueError(f'Package identity/hash mismatch: {asset["id"]}')
        records[asset['id']]=record
    if native:
        source=verify_source(args.manifest)[1]
        box=Sandbox()
        try:
            checks,commands,results,host=verify_macos(args,manifest,source,required,records,
                                                      provider,out,box)
        finally:
            box.cleanup()
        return write_report(args,manifest,out,checks,commands,results,image=None,host=host)
    checks,commands,results,image=verify_container(args,manifest,required,records,provider,out)
    return write_report(args,manifest,out,checks,commands,results,image=image)


def write_report(args, manifest, out, checks, commands, results, **environment):
    report={'schema_version':1,'target':args.target_id,'profile':manifest['profile'],
        'source_commit':manifest['source_commit'],'source_snapshot_sha256':manifest['source_snapshot_sha256'],
        'release_input_sha256':sha256_file(args.manifest),**environment,
        'status':'PASS' if checks and all(c['status']=='PASS' for c in checks) else 'FAIL',
        'checks':checks,'commands':commands,'results':results}
    write_json(out/'report.json',report)
    return report


def verify_container(args, manifest, required, records, provider, out):
    secret=provider['api_key']
    image=manifest['toolchain']['install_images'][args.target_id]
    checks=[]
    commands=[]
    results={}
    def execute(argv,log_name,timeout=600):
        result=subprocess.run(argv,capture_output=True,text=True,timeout=timeout)
        log=(result.stdout+result.stderr).replace(secret,'[REDACTED]')
        (out/log_name).write_text(log,encoding='utf-8')
        commands.append({'command':argv,'exit_code':result.returncode,'log':log_name})
        if result.returncode:
            raise ValueError(f'Container check failed. See {log_name}.')
        return result.stdout
    box=Sandbox()
    try:
        config={'config_version':3,'oobe_done':True,'active_provider':'opencodego',
            'active_provider_models':[{'provider_id':'opencodego','model':'deepseek-v4.1-flash'}],
            'providers':[provider],'memory':{'enabled':False}}
        config_path=box.root/'miyu/config/config.jsonc'
        write_json(config_path,config)
        config_path.chmod(0o600)
        name='miyu-verify-'+box.run_id[:12]
        probe=Path(__file__).resolve().parent/'probes/installed.py'
        base=['docker','exec',name]
        try:
            execute(['docker','run','--rm','-d','--name',name,
                '--label','io.miyu.distribution.owner=distribution-2026-09-14',
                '--mount',f'type=bind,src={args.packages.resolve()},dst=/packages,readonly',
                '--mount',f'type=bind,src={probe},dst=/probe.py,readonly',
                '--mount',f'type=bind,src={box.root},dst=/test-home',image,
                'sleep','infinity'],'container-start.txt',60)
            native=[r for r in records.values() if r['asset']['format']!='tar.gz']
            paths=[f'/packages/{r["asset"]["id"]}/{r["asset"]["filename"]}' for r in native]
            for index,command in enumerate(install_commands(args.target_id,paths)):
                execute(base+command,f'install-{index}.txt',900)
            execute(base+['uname','-m'],'architecture.txt',30)
            for asset_id,record in records.items():
                probe_home=box.root/'probes'/asset_id
                for child in ('home','miyu/config','runtime','config','data','cache','state'):
                    (probe_home/child).mkdir(parents=True,mode=0o700,exist_ok=True)
                write_json(probe_home/'miyu/config/config.jsonc',config)
                (probe_home/'miyu/config/config.jsonc').chmod(0o600)
                if record['asset']['format']=='tar.gz':
                    # Independent relocation test, away from /usr and all source assets.
                    prefix='/opt/Miyu Tar Test'
                    execute(base+['mkdir','-p',prefix],asset_id+'-mkdir.txt',30)
                    execute(base+['tar','-xzf',f'/packages/{asset_id}/{record["asset"]["filename"]}',
                                  '-C',prefix],asset_id+'-extract.txt',120)
                else:
                    prefix='/usr'
                output=execute(base+['python3','/probe.py','--record',f'/packages/{asset_id}/package-record.json',
                    '--prefix',prefix,'--version',manifest['version'],
                    '--revision',str(manifest['package_revision']),'--fedora-version',str(manifest['fedora_version']),
                    '--test-home',f'/test-home/probes/{asset_id}','--uid',str(os.getuid()),
                    '--gid',str(os.getgid())],asset_id+'-probe.txt',300)
                results[asset_id]=json.loads(output.strip().splitlines()[-1])
            for check in required:
                checks.append(dict(check,status='PASS',artifact_sha256=records[check['asset_id']]['sha256']))
        except (ValueError,OSError,subprocess.SubprocessError) as error:
            for check in required:
                checks.append(dict(check,status='FAIL',reason=str(error).replace(secret,'[REDACTED]'),
                    artifact_sha256=records[check['asset_id']]['sha256']))
        finally:
            try:
                cleanup_container(name,box,out,secret)
            except (ValueError,OSError,subprocess.SubprocessError) as error:
                checks=[dict(check,status='FAIL',reason=str(error).replace(secret,'[REDACTED]'),
                    artifact_sha256=records[check['asset_id']]['sha256']) for check in required]
    except BaseException:
        # Errors before container creation cannot leave a bind mount behind.
        if 'name' not in locals():
            box.cleanup()
        raise
    return checks,commands,results,image


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest',required=True,type=Path)
    parser.add_argument('--packages',required=True,type=Path)
    parser.add_argument('--target-id',required=True)
    parser.add_argument('--report-dir',required=True,type=Path)
    parser.add_argument('--provider-config',type=Path,help='Explicit local config. Only requested provider is copied temporarily.')
    parser.add_argument('--skip-provider',action='store_true',
                        help='macOS preview only: record provider-live as SKIPPED instead of calling a model.')
    parser.add_argument('--allow-homebrew-changes',action='store_true',
                        help='macOS only: let homebrew-formula install dependencies into this Homebrew.')
    args=parser.parse_args()
    try:
        report=verify(args)
        print(f'{report["status"]}: {args.target_id}. Report: {args.report_dir}/report.json')
        return 0 if report['status']=='PASS' else 1
    except BlockedError as error:
        print(f'BLOCKED: {error}',file=sys.stderr)
        return 3
    except (ValueError,KeyError,OSError,subprocess.SubprocessError) as error:
        print(f'ERROR: {error}',file=sys.stderr)
        return 1


if __name__=='__main__':
    sys.exit(main())

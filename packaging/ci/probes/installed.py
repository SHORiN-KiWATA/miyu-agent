#!/usr/bin/env python3
"""Installed-package probe. Mounted alone, without source assets or user home."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile

# 独立挂载进容器、不带 lib/,Mach-O 这几个常量就地写:64 位小端魔数与 arm64 的 cputype。
MACHO_ARM64 = (b'\xcf\xfa\xed\xfe', 0x0100000c)


def sha256(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream,'sha256').hexdigest()


def verify_files(prefix, records):
    for record in records:
        path=prefix/record['path']
        if record['type']=='symlink':
            if not path.is_symlink() or os.readlink(path)!=record['target']:
                raise ValueError(f'Installed symlink differs: {record["path"]}')
        elif record['type']=='directory':
            if not path.is_dir():
                raise ValueError(f'Installed directory missing: {record["path"]}')
        elif (not path.is_file() or path.is_symlink() or sha256(path)!=record['sha256']
                or path.stat().st_mode & 0o777 != int(record['mode'],8)):
            raise ValueError(f'Installed file content/mode differs: {record["path"]}')


def verify_package(record, package, version, revision, fedora):
    asset=record['asset']
    family=asset['format']
    name='miyu-voice' if asset['component']=='voice' else 'miyu'
    def output(argv):
        return subprocess.run(argv,check=True,capture_output=True,text=True,timeout=60).stdout
    if family=='deb':
        expected=[name,f'{version}-{revision}','amd64']
        actual=output(['dpkg-deb','-f',str(package),'Package','Version','Architecture']).splitlines()
        # Multiple requested fields are labelled by dpkg-deb.
        actual=[line.split(': ',1)[-1] for line in actual]
        installed=output(['dpkg-query','-W','-f=${Package}\n${Version}\n${Architecture}\n',name]).splitlines()
        with subprocess.Popen(['dpkg-deb','--fsys-tarfile',str(package)],stdout=subprocess.PIPE) as process:
            with tarfile.open(fileobj=process.stdout,mode='r|') as archive:
                members=[member.name for member in archive]
            if process.wait(timeout=30)!=0:raise ValueError('Cannot list DEB payload.')
    elif family=='rpm':
        expected=[name,f'{version}-{revision}.fc{fedora}','x86_64']
        query='%{NAME}\n%{VERSION}-%{RELEASE}\n%{ARCH}\n'
        actual=output(['rpm','-qp','--qf',query,str(package)]).splitlines()
        installed=output(['rpm','-q','--qf',query,name]).splitlines()
        members=output(['rpm','-qp','--qf','[%{FILENAMES}\n]',str(package)]).splitlines()
    elif family=='archlinux':
        info=output(['bsdtar','-xOf',str(package),'.PKGINFO'])
        fields=dict(line.split(' = ',1) for line in info.splitlines() if ' = ' in line)
        expected=[name,f'{version}-{revision}','x86_64']
        actual=[fields[k] for k in ('pkgname','pkgver','arch')]
        installed=output(['pacman','-Q',name]).split()+[fields['arch']]
        members=output(['bsdtar','-tf',str(package)]).splitlines()
    else:
        expected=actual=installed=None
        with tarfile.open(package,'r:gz') as archive:
            members=[member.name for member in archive]
    if actual!=expected or installed!=expected:
        raise ValueError(f'Package header/installed identity mismatch: {actual}, {installed}, expected {expected}')
    prefix='' if family=='tar.gz' else 'usr/'
    wanted={prefix+entry['path'] for entry in record.get('package_files',record['files'])}
    allowed={'','usr'} if prefix else {''}
    if family=='archlinux':allowed|={'.PKGINFO','.BUILDINFO','.MTREE'}
    normalized={str(Path(member)).removeprefix('/').removeprefix('./').rstrip('/') for member in members}
    normalized.discard('.')
    if normalized-wanted-allowed or wanted-normalized:
        raise ValueError(f'Package payload differs from manifest: extra={sorted(normalized-wanted-allowed)}, missing={sorted(wanted-normalized)}')


def verify_binary_format(binary, arch):
    with binary.open('rb') as stream:header=stream.read(20)
    if arch=='arm64':
        if header[:4]!=MACHO_ARM64[0] or int.from_bytes(header[4:8],'little')!=MACHO_ARM64[1]:
            raise ValueError('Installed binary is not an arm64 Mach-O executable.')
    elif header[:6]!=b'\x7fELF\x02\x01' or int.from_bytes(header[18:20],'little')!=62:
        raise ValueError('Installed binary is not an ELF64 x86_64 executable.')


def verify_functional(run, binary, prefix, files):
    """Homebrew 把包放进 Cellar 后,资源、技能与出厂脚本都要顺着程序所在前缀找得到(09-23)。"""
    listed=run([str(binary),'paths']).stdout
    personas=next((line.split(': ',1)[1] for line in listed.splitlines()
                   if line.startswith('system persona resources: ')),'')
    if not personas or Path(personas).resolve()!=(prefix/'share/miyu/personas').resolve():
        raise ValueError(f'Persona resources resolve outside the install prefix: {personas!r}')
    skills=sorted({m.group(1) for f in files
                   for m in [re.fullmatch(r'share/miyu/personas/[^/]+/skills/([^/]+)/SKILL\.md',f['path'])] if m})
    loaded=[]
    for name in skills:
        result=run([str(binary),'tool-call','load_skill',json.dumps({'name':name})])
        if result.returncode or name not in result.stdout:
            raise ValueError(f'Bundled skill did not load: {name}: '+result.stdout[-400:]+result.stderr[-400:])
        loaded.append(name)
    calc=run([str(binary),'tool-call','scientific_calculator',json.dumps({'expression':'6*7'})])
    if calc.returncode or '42' not in calc.stdout:
        raise ValueError('Bundled script did not run: '+calc.stdout[-400:]+calc.stderr[-400:])
    return {'persona_resources':personas,'skills_loaded':loaded,'script':'scientific_calculator'}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record',required=True,type=Path)
    parser.add_argument('--prefix',required=True,type=Path)
    parser.add_argument('--version',required=True)
    parser.add_argument('--revision',required=True,type=int)
    parser.add_argument('--fedora-version',required=True,type=int)
    parser.add_argument('--test-home',required=True,type=Path)
    parser.add_argument('--uid',required=True,type=int)
    parser.add_argument('--gid',required=True,type=int)
    parser.add_argument('--functional',action='store_true',help='Also load bundled skills and run a script.')
    parser.add_argument('--skip-provider',action='store_true',help='Preview only: no real model call.')
    args=parser.parse_args()
    if not args.prefix.is_absolute():
        parser.error('--prefix must be absolute')
    record=json.loads(args.record.read_text())
    package=args.record.parent/record['asset']['filename']
    verify_package(record,package,args.version,args.revision,args.fedora_version)
    verify_files(args.prefix,record['files'])
    component=record['asset']['component']
    binary=args.prefix/'bin'/('miyu-voice' if component=='voice' else 'miyu')
    env={'PATH':f'{args.prefix}/bin:/usr/bin:/bin','HOME':str(args.test_home/'home'),
         'MIYU_HOME':str(args.test_home/'miyu'),'XDG_RUNTIME_DIR':str(args.test_home/'runtime'),
         'XDG_CONFIG_HOME':str(args.test_home/'config'),'XDG_DATA_HOME':str(args.test_home/'data'),
         'XDG_CACHE_HOME':str(args.test_home/'cache'),'XDG_STATE_HOME':str(args.test_home/'state'),
         'LANG':'C.UTF-8','TERM':'dumb','RUST_LOG':'error'}
    def run(argv,timeout=60):
        return subprocess.run(argv,env=env,cwd='/tmp',capture_output=True,text=True,
            timeout=timeout,user=args.uid,group=args.gid)
    version=run([str(binary),'--version'])
    verify_binary_format(binary,record['asset']['arch'])
    if version.returncode or version.stdout.strip()!=f'{binary.name} {args.version}':
        raise ValueError('Installed binary version probe failed: '+version.stdout+version.stderr)
    result={'version':version.stdout.strip(),'files_verified':len(record['files']),
            'binary_sha256':sha256(binary),'asset_id':record['asset']['id']}
    if component=='core':
        help_result=run([str(binary),'--help'])
        if help_result.returncode or 'oobe' not in help_result.stdout.lower():
            raise ValueError('Installed help probe failed.')
        if args.functional:
            result['functional']=verify_functional(run,binary,args.prefix,record['files'])
    if component=='core' and not args.skip_provider:
        prompt='Say hello in one short sentence.'
        argv=[str(binary),'ask','--no-tools','--no-memory','--quiet','--output-format','json',
              '--model','opencodego/deepseek-v4.1-flash','--timeout','180',prompt]
        try:
            response=run(argv,timeout=210)
        finally:
            stopped=run([str(binary),'daemon','stop'],timeout=30)
            if stopped.returncode:
                raise ValueError('Test daemon did not stop cleanly: '+stopped.stdout+stopped.stderr)
        # Keep only final result fields; never serialize config or provider credentials.
        try:
            done=json.loads(response.stdout.strip().splitlines()[-1])
        except (ValueError,IndexError) as error:
            raise ValueError('Miyu did not return final JSON: '+response.stdout+response.stderr) from error
        if (response.returncode or done.get('type')!='done' or not done.get('text','').strip()
                or done.get('provider_id')!='opencodego'
                or done.get('model')!='deepseek-v4.1-flash'):
            raise ValueError('Real provider response failed: '+json.dumps(done,ensure_ascii=False))
        result['provider_live']={key:done.get(key) for key in
            ('type','text','provider_id','model','usage','elapsed_ms')}
    print(json.dumps(result,ensure_ascii=False))


if __name__=='__main__':
    try:
        main()
    except (OSError,ValueError,subprocess.SubprocessError) as error:
        print(f'FAIL: {error}',file=sys.stderr)
        sys.exit(1)

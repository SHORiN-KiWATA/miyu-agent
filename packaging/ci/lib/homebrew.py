"""Homebrew formula rendering. The repository copy is the truth source; the tap mirrors it.

和 AUR 的 PKGBUILD 一个待遇(09-23 用户拍板):版本、下载地址、哈希由验收过的发布包写入,
不手填。只替换这几行,formula 其余部分(依赖、安装、测试)照仓库里的原样发出去。
"""
import re

from .manifest import digest

FORMULA = 'packaging/homebrew/Formula/miyu.rb'
REPOSITORY = 'SHORiN-KiWATA/miyu-agent'
TAP = 'SHORiN-KiWATA/homebrew-miyu'
ASSET_ID = 'macos-core'


def release_url(tag, filename):
    return f'https://github.com/{REPOSITORY}/releases/download/{tag}/{filename}'


def formula_revision(package_revision):
    """包修订号 1 对应没有 revision 行;同版本重编(修订号 2、3…)要让 brew 认成新版本。"""
    if type(package_revision) is not int or package_revision < 1:
        raise ValueError('Package revision must be a positive integer.')
    return package_revision - 1


def render_formula(template, *, version, package_revision, url, sha256):
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise ValueError('Formula version must have three numeric components.')
    if not (url.startswith('https://') or url.startswith('file:///')) or '"' in url:
        raise ValueError('Formula URL must be an HTTPS release asset or a local file URL.')
    digest(sha256)
    result = template
    for field, value in (('url', url), ('version', version), ('sha256', sha256)):
        result, count = re.subn(rf'^(  {field} )"[^"\n]*"$', rf'\g<1>"{value}"', result,
                                count=1, flags=re.M)
        if count != 1:
            raise ValueError(f'Formula template is missing its {field} line.')
    result = re.sub(r'^  revision \d+\n', '', result, flags=re.M)
    revision = formula_revision(package_revision)
    if revision:
        result = re.sub(r'^(  sha256 "[0-9a-f]{64}"\n)', rf'\g<1>  revision {revision}\n',
                        result, count=1, flags=re.M)
    return result


def formula_fields(text):
    """Read back the rendered stanza. Tests and the tap push step compare these with the bundle."""
    fields = {}
    for field in ('url', 'version', 'sha256'):
        found = re.findall(rf'^  {field} "([^"\n]*)"$', text, flags=re.M)
        if len(found) != 1:
            raise ValueError(f'Formula must declare exactly one {field}.')
        fields[field] = found[0]
    revisions = re.findall(r'^  revision (\d+)$', text, flags=re.M)
    if len(revisions) > 1:
        raise ValueError('Formula declares more than one revision.')
    fields['revision'] = int(revisions[0]) if revisions else 0
    return fields

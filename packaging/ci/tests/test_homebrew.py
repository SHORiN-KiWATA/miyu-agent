"""Homebrew 渠道(09-23):formula 真相源的契约与渲染。

formula 由发版工具按验收过的包写入地址与哈希,和 AUR 的 PKGBUILD 同一个待遇;这里钉住
「只换那几行、其余原样」,以及仓库里那份模板本身和锁文件、包布局对得上。
"""
import hashlib
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import channel_update
from lib import homebrew
from lib.common import load_json

ROOT = Path(__file__).resolve().parents[3]
TEMPLATE = (ROOT/homebrew.FORMULA).read_text()
SHA = hashlib.sha256(b'macos tarball').hexdigest()
URL = homebrew.release_url('v0.6.3', 'miyu-0.6.3-1-aarch64-apple-darwin.tar.gz')


class RenderTests(unittest.TestCase):
    def render(self, **overrides):
        values = dict(version='0.6.3', package_revision=1, url=URL, sha256=SHA)
        values.update(overrides)
        return homebrew.render_formula(TEMPLATE, **values)

    def test_only_the_release_stanza_changes(self):
        rendered = self.render()
        self.assertEqual(homebrew.formula_fields(rendered),
                         {'url': URL, 'version': '0.6.3', 'sha256': SHA, 'revision': 0})
        changed = [(a, b) for a, b in zip(TEMPLATE.splitlines(), rendered.splitlines()) if a != b]
        self.assertEqual(len(TEMPLATE.splitlines()), len(rendered.splitlines()))
        self.assertEqual([a.split()[0] for a, _ in changed], ['url', 'version', 'sha256'])

    def test_package_revision_becomes_formula_revision(self):
        second = self.render(package_revision=2)
        self.assertEqual(homebrew.formula_fields(second)['revision'], 1)
        self.assertIn(f'  sha256 "{SHA}"\n  revision 1\n', second)
        # 再渲染一次回到修订号 1:旧的 revision 行要摘掉,不能叠两行。
        back = homebrew.render_formula(second, version='0.6.4', package_revision=1, url=URL, sha256=SHA)
        self.assertEqual(homebrew.formula_fields(back)['revision'], 0)
        third = homebrew.render_formula(second, version='0.6.3', package_revision=3, url=URL, sha256=SHA)
        self.assertEqual(third.count('  revision '), 1)

    def test_placeholders_and_unsafe_values_rejected(self):
        for overrides in ({'sha256': '0'*64}, {'sha256': 'abc'}, {'version': 'v0.6.3'},
                          {'url': 'http://example.com/miyu.tar.gz'}, {'url': URL+'"; system "x'},
                          {'package_revision': 0}):
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.render(**overrides)
        with self.assertRaises(ValueError):
            homebrew.render_formula(TEMPLATE.replace('  sha256 ', '  # sha256 '), version='0.6.3',
                                    package_revision=1, url=URL, sha256=SHA)

    def test_local_file_url_for_acceptance_installs(self):
        rendered = self.render(url='file:///tmp/miyu-release/macos/packages/macos-core/miyu.tar.gz')
        self.assertTrue(homebrew.formula_fields(rendered)['url'].startswith('file:///tmp/'))


class TemplateContractTests(unittest.TestCase):
    def test_template_matches_the_lock_and_the_package_layout(self):
        lock = load_json(ROOT/'packaging/common/toolchain.lock.json')['builders']['macos-arm64']
        # macOS 15 是 Sequoia;锁文件的部署目标改了,formula 的最低系统必须跟着改。
        self.assertEqual(lock['deployment_target'], '15.0')
        self.assertIn('  depends_on macos: :sequoia\n', TEMPLATE)
        self.assertIn('  depends_on arch: :arm64\n', TEMPLATE)
        for dependency in ('chafa', 'ripgrep', 'onnxruntime'):
            self.assertIn(f'  depends_on "{dependency}"\n', TEMPLATE)
        self.assertIn('    prefix.install "bin", "share"\n', TEMPLATE)
        fields = homebrew.formula_fields(TEMPLATE)
        self.assertEqual(fields['sha256'], '0'*64, '真哈希只能由 channel_update 写入')

    def test_tap_readme_installs_by_fully_qualified_name(self):
        readme = (ROOT/'packaging/homebrew/README.md').read_text()
        self.assertIn('brew install shorin-kiwata/miyu/miyu', readme)


class ChannelOutputTests(unittest.TestCase):
    def test_render_homebrew_writes_the_complete_tap_tree(self):
        manifest = {'version': '0.6.3', 'package_revision': 1, 'tag': 'v0.6.3'}
        record = {'filename': 'miyu-0.6.3-1-aarch64-apple-darwin.tar.gz', 'sha256': SHA}
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            patch_lines = list(channel_update.render_homebrew(manifest, record, out, apply=False))
            tap = out/'homebrew'
            self.assertEqual(sorted(p.relative_to(tap).as_posix() for p in tap.rglob('*') if p.is_file()),
                             ['Formula/miyu.rb', 'README.md'])
            self.assertEqual(homebrew.formula_fields((tap/'Formula/miyu.rb').read_text())['sha256'], SHA)
            self.assertTrue(any(line.startswith('+  sha256') for line in patch_lines))
        self.assertEqual((ROOT/homebrew.FORMULA).read_text(), TEMPLATE, 'apply=False 不能改仓库')

    def test_apply_updates_the_repository_truth_source(self):
        manifest = {'version': '0.6.3', 'package_revision': 2, 'tag': 'v0.6.3'}
        record = {'filename': 'miyu-0.6.3-2-aarch64-apple-darwin.tar.gz', 'sha256': SHA}
        with tempfile.TemporaryDirectory() as temp:
            repo = Path(temp)/'repo'
            (repo/'packaging/homebrew/Formula').mkdir(parents=True)
            (repo/homebrew.FORMULA).write_text(TEMPLATE)
            (repo/'packaging/homebrew/README.md').write_text('tap readme')
            with patch.object(channel_update, 'REPO', repo):
                list(channel_update.render_homebrew(manifest, record, Path(temp)/'out', apply=True))
            fields = homebrew.formula_fields((repo/homebrew.FORMULA).read_text())
        self.assertEqual((fields['version'], fields['revision']), ('0.6.3', 1))
        self.assertTrue(fields['url'].endswith('/v0.6.3/miyu-0.6.3-2-aarch64-apple-darwin.tar.gz'))


if __name__ == '__main__':
    unittest.main()

"""远端工作流与仓库之间的几处「必须对得上」。

这些都是实际绊过人的地方，不是假想：
  · `release.yml` 的 `notes-path` 默认值指向某一版的发布说明。版本号升了却忘了
    改它，远端一执行就去找一个不存在的文件——0.6.1 发版时就是这么发现 0.6.0
    那个默认值还留着的。
  · 两个 builder 镜像一行 `COPY` 都没有，构建上下文应当是空的。少了
    `.dockerignore`，`docker build … .` 会把整个仓库塞给守护进程；本机
    `target/` 实测 136 GB。
"""

import re
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[3]


def cargo_version():
    text = (ROOT / 'Cargo.toml').read_text(encoding='utf-8')
    return re.search(r'(?m)^version = "([^"]+)"', text).group(1)


class ReleaseNotesDefaultTests(unittest.TestCase):
    def setUp(self):
        self.version = cargo_version()
        self.workflow = (ROOT / '.github/workflows/release.yml').read_text(encoding='utf-8')

    def test_the_default_notes_path_tracks_the_cargo_version(self):
        found = re.findall(r'(?m)^\s*default: (docs/releases/\S+/release-notes\.md)\s*$',
                           self.workflow)
        self.assertEqual(len(found), 1, '发布说明默认值只应有一处')
        self.assertEqual(found[0], f'docs/releases/{self.version}/release-notes.md')

    def test_the_notes_and_the_archived_changelog_both_exist(self):
        for name in ('release-notes.md', 'changelog.md'):
            path = ROOT / 'docs/releases' / self.version / name
            self.assertTrue(path.is_file(), f'缺少 {path.relative_to(ROOT)}')
            self.assertGreater(len(path.read_text(encoding='utf-8').strip()), 0, name)


class BuildContextTests(unittest.TestCase):
    def test_the_builder_images_take_no_build_context(self):
        for name in ('Dockerfile.gnu', 'Dockerfile.arch'):
            text = (ROOT / 'packaging/linux/builders' / name).read_text(encoding='utf-8')
            for line in text.splitlines():
                self.assertFalse(line.strip().upper().startswith(('COPY ', 'ADD ')),
                                 f'{name} 开始依赖构建上下文了，.dockerignore 得跟着改')

    def test_dockerignore_excludes_everything(self):
        path = ROOT / '.dockerignore'
        self.assertTrue(path.is_file(), '缺少 .dockerignore：构建上下文会带上整个 target/')
        entries = [line.strip() for line in path.read_text(encoding='utf-8').splitlines()
                   if line.strip() and not line.startswith('#')]
        self.assertEqual(entries, ['*'])


class MacosPackageWorkflowTests(unittest.TestCase):
    """macOS 包在 GitHub runner 上构建(09-23)。推候选分支触发:分支名就是发版请求。"""

    def setUp(self):
        sys.path.insert(0, str(ROOT/'packaging/ci'))
        from workflow import macos_parameters
        self.parameters = macos_parameters
        self.workflow = (ROOT/'.github/workflows/macos-package.yml').read_text(encoding='utf-8')

    def test_actions_are_pinned_like_the_other_workflows(self):
        release = (ROOT/'.github/workflows/release.yml').read_text(encoding='utf-8')
        pins = set(re.findall(r'uses: (\S+@[0-9a-f]{40})', release))
        used = set(re.findall(r'uses: (\S+)', self.workflow))
        self.assertTrue(used)
        self.assertLessEqual(used, pins, 'macOS 工作流用了没钉过 SHA 的 action')

    def test_candidate_branch_names_are_the_release_request(self):
        self.assertIn("branches: ['release/v*', 'macos-preview/**']", self.workflow)
        self.assertEqual(self.parameters('push', 'release/v0.6.3-2', {}),
                         {'mode': 'release', 'tag': 'v0.6.3', 'revision': '2', 'expect': ''})
        self.assertEqual(self.parameters('push', 'macos-preview/homebrew', {})['mode'], 'preview')
        for event, ref in (('push', 'release/v0.6.3'), ('push', 'release/v0.6.3-0'),
                           ('push', 'release/0.6.3-1'), ('push', 'main'), ('pull_request', 'x')):
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                self.parameters(event, ref, {})

    def test_dispatch_inputs_are_validated(self):
        ok = {'mode': 'release', 'tag': 'v0.6.3', 'revision': '1', 'expect': 'a'*64}
        self.assertEqual(self.parameters('workflow_dispatch', 'main', ok)['tag'], 'v0.6.3')
        for bad in ({'mode': 'release', 'tag': '', 'revision': '1'},
                    {'mode': 'preview', 'tag': 'v0.6.3', 'revision': '1'},
                    {'mode': 'release', 'tag': 'v0.6.3', 'revision': '0'},
                    {'mode': 'release', 'tag': 'v0.6.3', 'revision': '1', 'expect': 'nothex'}):
            with self.subTest(inputs=bad), self.assertRaises(ValueError):
                self.parameters('workflow_dispatch', 'main', bad)

    def test_provider_secret_only_reaches_the_verify_step(self):
        steps = self.workflow.split('      - ')
        with_secret = [step for step in steps if 'secrets.' in step]
        self.assertEqual(len(with_secret), 1)
        self.assertIn('macos-verify', with_secret[0])


if __name__ == '__main__':
    unittest.main()

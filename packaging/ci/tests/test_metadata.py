import copy
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib.common import canonical_json, load_json
from lib.manifest import COMMON, validate_manifest
from lib.matrix import release_matrix, selected_targets, TARGET_IDS
from lib.source import export_source, source_files, snapshot_digest


def fixture(profile='stable-full'):
    """Synthetic manifest for contract tests, never a release verification report."""
    sha1 = hashlib.sha1(b'test source').hexdigest()
    sha256 = hashlib.sha256(b'test input').hexdigest()
    lock = load_json(COMMON/'toolchain.lock.json')
    builds, assets, checks = release_matrix(COMMON/'targets.json', profile,
        list(TARGET_IDS), '0.6.0', 1, lock['fedora']['version'])
    return {'schema_version': 1, 'mode': 'release', 'profile': profile,
        'version': '0.6.0', 'package_revision': 1, 'tag': 'v0.6.0', 'tag_commit': sha1,
        'source_commit': sha1, 'source_snapshot_sha256': sha256, 'source_dirty': False,
        'source_date_epoch': 1789316605, 'wiki_commit': sha1, 'workflow_commit': sha1,
        'locks': {k: sha256 for k in ('cargo','toolchain','third_party','assets','targets')},
        'toolchain': lock, 'builders': lock['builders'], 'fedora_version': lock['fedora']['version'],
        'targets': list(TARGET_IDS), 'builds': builds, 'assets': assets, 'checks': checks,
        'release_declaration': None, 'channels': {'github': 'stable','macos_direct_signing': 'required'}}


class ManifestTests(unittest.TestCase):
    def test_complete_matrix_and_stable_serialization(self):
        manifest = fixture()
        self.assertEqual(len(validate_manifest(manifest)['targets']), 7)
        self.assertEqual(len(manifest['builds']), 3)
        self.assertEqual(len(manifest['assets']), 10)
        self.assertEqual(canonical_json(manifest), canonical_json(copy.deepcopy(manifest)))
        self.assertEqual({c['target'] for c in manifest['checks']}, set(TARGET_IDS))

    def test_tampered_contracts_rejected(self):
        mutations = (
            lambda m: m.update(version='0.7.0'),
            lambda m: m.update(source_commit='HEAD'),
            lambda m: m['targets'].pop(),
            lambda m: m['assets'][1].update(filename=m['assets'][0]['filename']),
            lambda m: m['assets'][0].update(arch='aarch64'),
            lambda m: m['locks'].update(cargo='0'*64),
            lambda m: m['checks'].pop(),
            lambda m: m['checks'][0].update(required=False),
            lambda m: m.update(source_dirty=True),
            lambda m: m.update(schema_version=True),
            lambda m: m['channels'].update(macos_direct_signing='unsigned-preview'),
            lambda m: m['toolchain']['builders']['gnu-x86_64'].update(image='debian:13'),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                manifest = fixture()
                mutate(manifest)
                with self.assertRaises(ValueError):
                    validate_manifest(manifest)

    def test_stable_subset_and_unknown_target_rejected(self):
        with self.assertRaises(ValueError):
            selected_targets('stable-core', ['debian13-x86_64'])
        with self.assertRaises(ValueError):
            selected_targets('preview-core', ['linux-arm64'])
        self.assertEqual(selected_targets('preview-core', ['debian13-x86_64']), ['debian13-x86_64'])

    def test_tag_mismatch_requires_declaration(self):
        manifest = fixture()
        manifest['tag_commit'] = hashlib.sha1(b'previous tag').hexdigest()
        with self.assertRaises(ValueError):
            validate_manifest(manifest)
        manifest['release_declaration'] = {k: manifest[k] for k in ('tag_commit','source_commit')}
        manifest['release_declaration']['reason'] = 'Explicit packaging revision test fixture'
        validate_manifest(manifest)

    def test_user_linux_smoke_profile_freezes_all_six_install_targets(self):
        selected = selected_targets('linux-smoke', None)
        self.assertEqual(len(selected), 6)
        # Mint 22 与两个 Ubuntu、Debian 13 共用同一个 DEB:多的是安装验收目标,
        # 不是多一个附件。
        self.assertIn('mint22-x86_64', selected)
        self.assertNotIn('macos-arm64', selected)
        with self.assertRaises(ValueError):
            selected_targets('linux-smoke', ['debian13-x86_64'])
        builds, assets, checks = release_matrix(COMMON/'targets.json', 'linux-smoke',
                                                selected, '0.6.0', 1, 44)
        self.assertEqual(len(assets), 8)
        self.assertEqual(set(builds), {'gnu-x86_64', 'arch-x86_64'})
        self.assertEqual({c['target'] for c in checks if c['check']=='provider-live'}, set(selected))


class SmokeProfileTests(unittest.TestCase):
    """09-23:macOS 主程序进公开发布(Homebrew),语音不进;Linux 那六个目标一点不变。"""

    def test_smoke_adds_only_macos_core_to_linux_smoke(self):
        linux = release_matrix(COMMON/'targets.json', 'linux-smoke', selected_targets('linux-smoke', None),
                               '0.6.0', 1, 44)
        selected = selected_targets('smoke', None)
        self.assertEqual(selected, list(TARGET_IDS))
        builds, assets, checks = release_matrix(COMMON/'targets.json', 'smoke', selected, '0.6.0', 1, 44)
        self.assertEqual(builds['macos-arm64']['components'], ['core'])
        self.assertEqual({a['id'] for a in assets} - {a['id'] for a in linux[1]}, {'macos-core'})
        mac = [c['check'] for c in checks if c['target'] == 'macos-arm64']
        self.assertEqual(mac, ['artifact-identity', 'package-install', 'assets-complete',
                               'provider-live', 'homebrew-formula'])
        self.assertEqual([c for c in checks if c['target'] != 'macos-arm64'], linux[2])
        with self.assertRaises(ValueError):
            selected_targets('smoke', ['macos-arm64'])

    def test_smoke_manifest_validates_in_release_and_preview(self):
        manifest = fixture('smoke')
        manifest['channels']['macos_direct_signing'] = 'homebrew-unsigned'
        validate_manifest(manifest)
        manifest['channels']['macos_direct_signing'] = 'required'
        with self.assertRaises(ValueError):
            validate_manifest(manifest)
        preview = fixture('smoke')
        preview['channels']['macos_direct_signing'] = 'homebrew-unsigned'
        preview.update(mode='preview', tag=None, tag_commit=None)
        validate_manifest(preview)


class SourceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name)/'source'
        self.repo.mkdir()
        subprocess.run(['git','init','-q',str(self.repo)], check=True)
        (self.repo/'.gitignore').write_text('ignored/\n')
        (self.repo/'tracked.txt').write_text('first')
        subprocess.run(['git','-C',str(self.repo),'add','.'], check=True)

    def tearDown(self):
        self.temp.cleanup()

    def test_snapshot_tracks_bytes_modes_deletions_and_explicit_new_files(self):
        initial = source_files(self.repo)
        (self.repo/'new.txt').write_text('new')
        self.assertEqual(initial, source_files(self.repo))
        included = source_files(self.repo, ['new.txt'])
        self.assertNotEqual(snapshot_digest(initial), snapshot_digest(included))
        (self.repo/'tracked.txt').chmod(0o755)
        self.assertNotEqual(snapshot_digest(initial), snapshot_digest(source_files(self.repo)))
        (self.repo/'tracked.txt').unlink()
        self.assertNotIn('tracked.txt', [r['path'] for r in source_files(self.repo)])

    def test_snapshot_export_and_tamper_refusal(self):
        records = source_files(self.repo)
        destination = Path(self.temp.name)/'export'
        self.assertEqual(export_source(self.repo, destination, records), snapshot_digest(records))
        self.assertEqual((destination/'tracked.txt').read_text(), 'first')
        with self.assertRaises(ValueError):
            export_source(self.repo, destination, records)
        (self.repo/'tracked.txt').write_text('changed while exporting')
        with self.assertRaises(ValueError):
            export_source(self.repo, Path(self.temp.name)/'changed', records)

    def test_ignored_escape_symlink_and_directory_rejected(self):
        (self.repo/'ignored').mkdir()
        (self.repo/'ignored/secret').write_text('do not export')
        (self.repo/'escape').symlink_to('/etc/passwd')
        for value in ('ignored/secret','../outside','/etc/passwd','escape','ignored'):
            with self.subTest(path=value), self.assertRaises(ValueError):
                source_files(self.repo, [value])


if __name__ == '__main__':
    unittest.main()

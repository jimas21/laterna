"""Tests for frameplayerx-desktop-install.py: python3 -m unittest (in this folder)."""

import importlib.util
import os
import stat
import struct
import sys
import tempfile
import unittest
import zipfile
import zlib

spec = importlib.util.spec_from_file_location(
    'fai', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'frameplayerx-desktop-install.py'))
fai = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fai)


def other(name, appid=-5):
    return [['appid', appid], ['AppName', name], ['Exe', f'"/usr/bin/{name}"'],
            ['tags', [['0', 'Games']]]]


class Vdf(unittest.TestCase):
    def test_round_trip_keeps_every_byte(self):
        doc = [['shortcuts', [['0', other('a') + [['f', ('raw', 3, b'\x00\x00\x80?')],
                                                   ['u', ('raw', 7, b'\x01' * 8)]]],
                              ['1', other('b', 7)]]]]
        data = fai.vdf_write(doc)
        self.assertEqual(fai.vdf_parse(data), doc)
        self.assertEqual(fai.vdf_write(fai.vdf_parse(data)), data)
        self.assertTrue(data.startswith(b'\x00shortcuts\x00\x000\x00\x02appid\x00'))
        self.assertTrue(data.endswith(b'\x08\x08\x08'))

    def test_refuses_unknown_types_and_truncation(self):
        with self.assertRaises(fai.Fail):
            fai.vdf_parse(b'\x00shortcuts\x00\x05x\x00')
        with self.assertRaises(fai.Fail):
            fai.vdf_parse(b'\x00shortcuts\x00\x01AppName\x00abc\x00')

    def test_empty_file(self):
        self.assertEqual(fai.vdf_parse(b''), [])


class Shortcut(unittest.TestCase):
    exe = '/home/steamos/frameplayerx/frameplayerx.sh'

    def test_app_id_matches_frameplayerx_install(self):
        # Same formula and vector as shortcut.rs: crc32("123456789") = 0xCBF43926.
        self.assertEqual(zlib.crc32(b'123456789') | 0x80000000, 0xCBF43926)
        self.assertEqual(fai.app_id('Laterna', self.exe),
                         zlib.crc32(f'"{self.exe}"Laterna'.encode()) | 0x80000000)

    def test_add_update_remove(self):
        doc = [['shortcuts', [['0', other('a')], ['1', other('b')]]]]
        aid, added = fai.upsert(doc, 'Laterna', self.exe, '/home/steamos/frameplayerx', '/i.png')
        self.assertTrue(added)
        doc = fai.vdf_parse(fai.vdf_write(doc))
        e = fai.get(fai.get(doc, 'shortcuts'), '2')
        self.assertEqual(fai.get(e, 'appid'), fai.signed(aid))
        self.assertEqual(fai.get(e, 'Exe'), f'"{self.exe}"')
        self.assertEqual(fai.get(e, 'StartDir'), '"/home/steamos/frameplayerx"')
        self.assertEqual(fai.get(fai.get(e, 'tags'), '0'), 'VR')

        # Update keeps what Steam and the user own, adds no duplicate.
        fai.put(e, 'LastPlayTime', 1700000000)
        fai.put(e, 'IsHidden', 1)
        _, added = fai.upsert(doc, 'Laterna', self.exe, '/home/steamos/frameplayerx', '/i.png')
        self.assertFalse(added)
        sc = fai.get(doc, 'shortcuts')
        self.assertEqual(len(sc), 3)
        self.assertEqual(fai.get(e, 'LastPlayTime'), 1700000000)
        self.assertEqual(fai.get(e, 'IsHidden'), 1)
        self.assertEqual(len(fai.get(e, 'tags')), 1)

        # A copy renamed in Steam is still found by its Exe.
        _, added = fai.upsert(doc, 'Laterna Beta', self.exe, '/home/steamos/frameplayerx', '')
        self.assertFalse(added)

        self.assertTrue(fai.remove_entries(doc, 'nope', self.exe))
        self.assertFalse(fai.remove_entries(doc, 'nope', self.exe))
        self.assertEqual([k for k, _ in sc], ['0', '1'])
        self.assertEqual(fai.get(fai.get(sc, '1'), 'AppName'), 'b')

    def test_entry_under_the_old_name_is_renamed_not_duplicated(self):
        doc = []
        old_id, _ = fai.upsert(doc, 'FramePlayerX', self.exe, '/home/steamos/frameplayerx', '')
        new_id, added = fai.upsert(doc, 'Laterna', self.exe, '/home/steamos/frameplayerx', '')
        self.assertFalse(added)
        self.assertNotEqual(old_id, new_id)
        sc = fai.get(doc, 'shortcuts')
        self.assertEqual(len(sc), 1)
        e = fai.get(sc, '0')
        self.assertEqual(fai.get(e, 'AppName'), 'Laterna')
        self.assertEqual(fai.get(e, 'appid'), fai.signed(new_id))

    def test_test_copy_is_separate(self):
        doc = []
        fai.upsert(doc, 'Laterna', self.exe, '/x', '')
        _, added = fai.upsert(doc, 'Laterna (test)', '/home/steamos/Apps/frameplayerx/frameplayerx.sh',
                              '/y', '')
        self.assertTrue(added)
        self.assertEqual(len(fai.get(doc, 'shortcuts')), 2)

    def test_shortcut_file_is_backed_up_and_replaced(self):
        with tempfile.TemporaryDirectory() as cfg:
            path = os.path.join(cfg, 'shortcuts.vdf')
            with open(path, 'wb') as f:
                f.write(fai.vdf_write([['shortcuts', [['0', other('a')]]]]))
            fai.write_shortcut_file(cfg, lambda d: fai.upsert(d, 'X', '/x.sh', '/', ''))
            names = os.listdir(cfg)
            self.assertTrue(any(n.startswith('shortcuts.vdf.bak-') for n in names))
            with open(path, 'rb') as f:
                self.assertEqual(len(fai.get(fai.vdf_parse(f.read()), 'shortcuts')), 2)


class OldVersions(unittest.TestCase):
    app = fai.APPS['frameplayerx']

    def copy(self, path, launcher='frameplayerx.sh'):
        os.makedirs(path, exist_ok=True)
        with open(os.path.join(path, launcher), 'w') as f:
            f.write('#!/bin/sh\n')
        return os.path.realpath(path)

    def test_finds_every_other_copy(self):
        with tempfile.TemporaryDirectory() as home:
            dest = self.copy(os.path.join(home, 'frameplayerx'))
            build3 = self.copy(dest + '.release-build3')
            old = self.copy(dest + '.old')
            keep = self.copy(dest + '.keep')
            devkit = self.copy(os.path.join(home, 'devkit-game', 'FramePlayerX'), 'frameplayerx.sh')
            test = self.copy(os.path.join(home, 'Apps', 'frameplayerx'))
            notes = os.path.join(home, 'frameplayerx.notes')  # not a leftover name
            self.copy(notes)
            entries = [
                (1, 'Laterna', os.path.join(dest, 'frameplayerx.sh')),  # this one: kept
                (2, 'Laterna (old)', os.path.join(build3, 'frameplayerx.sh')),
                (3, 'Laterna', os.path.join(devkit, 'frameplayerx.sh')),  # by launcher
                (4, 'Laterna (test)', os.path.join(test, 'frameplayerx.sh')),
                (5, 'OtherApp', os.path.join(home, 'other', 'other.sh')),
                (6, 'Laterna', '/usr/bin/elsewhere.sh'),  # by name, folder not ours
                (7, 'Laterna', ''),  # launcher unknown (Steam's live list): this one
                (8, 'Laterna Beta', ''),  # launcher unknown, another name
            ]
            entries_old, folders = fai.old_versions(self.app, dest, entries, home)
            self.assertEqual(sorted(a for a, _, _ in entries_old), [2, 3, 4, 6, 8])
            self.assertEqual(folders, sorted([build3, old, devkit, test]))
            self.assertNotIn(keep, folders)
            self.assertNotIn(dest, folders)

    def test_entries_under_the_old_name_count_as_copies(self):
        with tempfile.TemporaryDirectory() as home:
            dest = self.copy(os.path.join(home, 'frameplayerx'))
            entries = [
                (1, 'FramePlayerX', os.path.join(dest, 'frameplayerx.sh')),  # this one: kept
                (2, 'FramePlayerX Beta', ''),  # another copy, launcher unknown
            ]
            old, _ = fai.old_versions(self.app, dest, entries, home)
            self.assertEqual([a for a, _, _ in old], [2])

    def test_ignores_folders_without_the_app_or_outside_home(self):
        with tempfile.TemporaryDirectory() as home, tempfile.TemporaryDirectory() as other:
            dest = os.path.join(home, 'frameplayerx')
            os.makedirs(dest + '.old')  # empty: not a copy
            outside = self.copy(os.path.join(other, 'fp'))
            _, folders = fai.old_versions(self.app, dest, [(9, 'Laterna', os.path.join(outside, 'frameplayerx.sh'))], home)
            self.assertEqual(folders, [])

    def test_never_takes_a_shared_folder_for_a_copy(self):
        with tempfile.TemporaryDirectory() as home:
            # A launcher left loose in Downloads, and the install inside a
            # folder that also holds one.
            downloads = self.copy(os.path.join(home, 'Downloads'))
            outer = self.copy(os.path.join(home, 'frameplayer-things'))
            dest = os.path.join(outer, 'frameplayerx')
            entries = [
                (2, 'Laterna', os.path.join(downloads, 'frameplayerx.sh')),
                (3, 'Laterna (old)', os.path.join(outer, 'frameplayerx.sh')),
            ]
            old, folders = fai.old_versions(self.app, dest, entries, home)
            self.assertEqual(sorted(a for a, _, _ in old), [2, 3])  # the entries go
            self.assertEqual(folders, [])  # the folders stay

    def test_running_matches_the_binary_exactly(self):
        with tempfile.TemporaryDirectory() as home, tempfile.TemporaryDirectory() as proc:
            dest = os.path.realpath(self.copy(os.path.join(home, 'frameplayerx')))
            other = os.path.realpath(self.copy(os.path.join(home, 'frameplayerx.old')))
            os.makedirs(os.path.join(proc, 'self'))
            os.makedirs(os.path.join(proc, '41'))
            os.symlink(os.path.join(other, 'frameplayerx'), os.path.join(proc, '41', 'exe'))
            self.assertFalse(fai.running(self.app, dest, proc))
            self.assertTrue(fai.running(self.app, other, proc))
            # Replaced on disk while it runs.
            os.makedirs(os.path.join(proc, '42'))
            os.symlink(os.path.join(dest, 'frameplayerx') + ' (deleted)',
                       os.path.join(proc, '42', 'exe'))
            self.assertTrue(fai.running(self.app, dest, proc))

    def test_remove_appids(self):
        doc = [['shortcuts', [['0', other('a', fai.signed(0x8f000001))], ['1', other('b', 7)],
                              ['2', other('c', 8)]]]]
        self.assertTrue(fai.remove_appids(doc, {0x8f000001, 8}))
        sc = fai.get(doc, 'shortcuts')
        self.assertEqual([(k, fai.get(v, 'AppName')) for k, v in sc], [('0', 'b')])
        self.assertFalse(fai.remove_appids(doc, {99}))


class Unzip(unittest.TestCase):
    def make_zip(self, path, entries):
        with zipfile.ZipFile(path, 'w') as z:
            for name, mode, data in entries:
                info = zipfile.ZipInfo(name)
                info.create_system = 3
                info.external_attr = (mode << 16) | (0x10 if name.endswith('/') else 0)
                z.writestr(info, data)

    @unittest.skipIf(sys.platform == 'win32', 'POSIX permissions not supported on Windows')
    def test_keeps_exec_bits_and_strips_single_top_folder(self):
        with tempfile.TemporaryDirectory() as d:
            zp = os.path.join(d, 'a.zip')
            self.make_zip(zp, [('frameplayerx/', 0o40755, b''),
                               ('frameplayerx/frameplayerx.sh', 0o100755, b'#!/bin/sh\n'),
                               ('frameplayerx/README.txt', 0o100644, b'hi')])
            dest = os.path.join(d, 'out')
            fai.extract(zp, dest)
            self.assertTrue(os.stat(os.path.join(dest, 'frameplayerx.sh')).st_mode & stat.S_IXUSR)
            self.assertFalse(os.stat(os.path.join(dest, 'README.txt')).st_mode & stat.S_IXUSR)

    def test_flat_zip_is_not_stripped(self):
        with tempfile.TemporaryDirectory() as d:
            zp = os.path.join(d, 'c.zip')
            self.make_zip(zp, [('custom.sh', 0o100755, b'x'), ('bin/prog', 0o100755, b'y')])
            dest = os.path.join(d, 'out')
            fai.extract(zp, dest)
            self.assertTrue(os.path.isfile(os.path.join(dest, 'custom.sh')))
            self.assertTrue(os.path.isfile(os.path.join(dest, 'bin/prog')))

    def test_rejects_paths_outside(self):
        with tempfile.TemporaryDirectory() as d:
            zp = os.path.join(d, 'bad.zip')
            self.make_zip(zp, [('ok.txt', 0o100644, b''), ('../evil.sh', 0o100755, b'')])
            with self.assertRaises(fai.Fail):
                fai.extract(zp, os.path.join(d, 'out'))
            self.assertFalse(os.path.exists(os.path.join(d, 'evil.sh')))

    def test_swap_keeps_previous_version(self):
        with tempfile.TemporaryDirectory() as d:
            zp = os.path.join(d, 'a.zip')
            self.make_zip(zp, [('frameplayerx/frameplayerx.sh', 0o100755, b'v2')])
            dest = os.path.join(d, 'frameplayerx')
            os.makedirs(dest)
            with open(os.path.join(dest, 'frameplayerx.sh'), 'w') as f:
                f.write('v1')
            fai.install_files(fai.APPS['frameplayerx'], zp, dest)
            for path, want in ((dest, 'v2'), (dest + '.old', 'v1')):
                with open(os.path.join(path, 'frameplayerx.sh')) as f:
                    self.assertEqual(f.read(), want)
            self.assertFalse(os.path.exists(dest + '.new'))
            self.assertFalse(os.path.exists(dest + '.staged'))

    def test_a_download_without_the_app_changes_nothing(self):
        with tempfile.TemporaryDirectory() as d:
            zp = os.path.join(d, 'a.zip')
            self.make_zip(zp, [('frameplayerx/README.txt', 0o100644, b'hi')])
            dest = os.path.join(d, 'frameplayerx')
            os.makedirs(dest)
            with open(os.path.join(dest, 'frameplayerx.sh'), 'w') as f:
                f.write('v1')
            with self.assertRaises(fai.Fail):
                fai.install_files(fai.APPS['frameplayerx'], zp, dest)
            with open(os.path.join(dest, 'frameplayerx.sh')) as f:
                self.assertEqual(f.read(), 'v1')
            self.assertEqual(sorted(os.listdir(d)), ['a.zip', 'frameplayerx'])

    def test_a_failed_swap_puts_the_installed_version_back(self):
        with tempfile.TemporaryDirectory() as d:
            zp = os.path.join(d, 'a.zip')
            self.make_zip(zp, [('frameplayerx/frameplayerx.sh', 0o100755, b'v2')])
            dest = os.path.join(d, 'frameplayerx')
            os.makedirs(dest)
            with open(os.path.join(dest, 'frameplayerx.sh'), 'w') as f:
                f.write('v1')
            rename = os.rename

            def failing(src, dst):
                if src.endswith('.staged'):
                    raise OSError('disk full')
                rename(src, dst)

            fai.os.rename = failing
            try:
                with self.assertRaises(fai.Fail):
                    fai.install_files(fai.APPS['frameplayerx'], zp, dest)
            finally:
                fai.os.rename = rename
            with open(os.path.join(dest, 'frameplayerx.sh')) as f:
                self.assertEqual(f.read(), 'v1')
            self.assertEqual(sorted(os.listdir(d)), ['a.zip', 'frameplayerx'])

    def test_an_interrupted_swap_is_put_right(self):
        with tempfile.TemporaryDirectory() as d:
            dest = os.path.join(d, 'frameplayerx')
            os.makedirs(dest + '.old')
            with open(os.path.join(dest + '.old', 'frameplayerx.sh'), 'w') as f:
                f.write('v1')
            fai.recover(fai.APPS['frameplayerx'], dest)
            with open(os.path.join(dest, 'frameplayerx.sh')) as f:
                self.assertEqual(f.read(), 'v1')
            self.assertFalse(os.path.exists(dest + '.old'))
            # An installed copy is left as it is.
            os.makedirs(dest + '.old')
            fai.recover(fai.APPS['frameplayerx'], dest)
            self.assertTrue(os.path.isdir(dest + '.old'))


def release(tag, zip_file=True, draft=False):
    version = tag.lstrip('v')
    assets = [{'name': 'manifest.json', 'browser_download_url': 'https://example.com/m',
               'size': 1}]
    if zip_file:
        assets.append({'name': f'frameplayerx-{version}-aarch64.zip',
                       'browser_download_url': f'https://example.com/{tag}.zip',
                       'size': 123, 'digest': f'sha256:{version}'})
    return {'tag_name': tag, 'draft': draft, 'assets': assets}


class ReleaseDiscovery(unittest.TestCase):
    app = fai.APPS['frameplayerx']

    def test_stable_by_version_betas_only_when_asked(self):
        # Listed newest first, as GitHub does: a beta after the stable
        # release, and a newer stable release still uploading its zip.
        found = [release('v0.6.0-beta.10'), release('v0.6.0-beta.9'), release('v0.5.1', zip_file=False),
                 release('v0.5.0'), release('nightly'), release('v0.4.0')]
        tag, name, url, size, digest = fai.latest_release(self.app, found)
        self.assertEqual((tag, name, digest), ('v0.5.0', 'frameplayerx-0.5.0-aarch64.zip', '0.5.0'))
        self.assertEqual(fai.latest_release(self.app, found, beta=True)[0], 'v0.6.0-beta.10')
        found.append(release('v0.6.0'))
        self.assertEqual(fai.latest_release(self.app, found, beta=True)[0], 'v0.6.0')

    def test_fails_without_a_matching_release(self):
        with self.assertRaises(fai.Fail):
            fai.latest_release(self.app, [release('v1.0.0-beta.1')])
        with self.assertRaises(fai.Fail):
            fai.latest_release(self.app, [release('v1.0.0', zip_file=False)], beta=True)

    def test_version_order(self):
        tags = ['v1.0.0', 'v1.0.0-beta.2', 'v0.9.10', 'v1.0.0-alpha', 'v0.9.9', 'v1.0.0-beta.10']
        self.assertEqual(sorted(tags, key=fai.version_key),
                         ['v0.9.9', 'v0.9.10', 'v1.0.0-alpha', 'v1.0.0-beta.2', 'v1.0.0-beta.10', 'v1.0.0'])
        self.assertIsNone(fai.version_key('nightly'))

    def test_releases_drops_drafts_and_reports_errors(self):
        import io
        import json
        from unittest.mock import patch

        listed = json.dumps([release('v2.0.0', draft=True), release('v1.0.0')]).encode()
        with patch('urllib.request.urlopen', return_value=io.BytesIO(listed)):
            self.assertEqual([r['tag_name'] for r in fai.releases('a/b')], ['v1.0.0'])
        with patch('urllib.request.urlopen', return_value=io.BytesIO(b'{"message": "rate limited"}')):
            with self.assertRaises(fai.Fail):
                fai.releases('a/b')
        with patch('urllib.request.urlopen', side_effect=OSError('offline')):
            with self.assertRaises(fai.Fail):
                fai.releases('a/b')

    def test_published_digest(self):
        found = [release('v1.0.0'), release('v0.9.0-beta.1')]
        self.assertEqual(fai.published_digest(found, 'frameplayerx-0.9.0-beta.1-aarch64.zip'), '0.9.0-beta.1')
        self.assertIsNone(fai.published_digest(found, 'frameplayerx-0.1.0-aarch64.zip'))


class DesktopLauncher(unittest.TestCase):
    def exec_line(self, launcher):
        here = os.path.dirname(os.path.abspath(__file__))
        with open(os.path.join(here, launcher)) as f:
            text = f.read()
        self.assertTrue(text.startswith('[Desktop Entry]\n'))
        line = next(line for line in text.splitlines() if line.startswith('Exec='))
        url = f'https://github.com/{fai.REPO}/releases/latest/download/frameplayerx-desktop-install.py'
        self.assertIn(url, line)
        self.assertTrue(os.path.isfile(os.path.join(here, url.rsplit('/', 1)[1])))
        # Characters the desktop entry spec would need escaped inside quotes.
        self.assertFalse(any(c in line for c in '$`\\%'))
        return line

    def test_install_and_uninstall_run_this_installer_from_the_latest_release(self):
        self.assertTrue(self.exec_line('Install-FramePlayerX.desktop').endswith('install.py"'))
        self.assertTrue(self.exec_line('Uninstall-FramePlayerX.desktop').endswith('install.py --remove"'))


class Uninstall(unittest.TestCase):
    exe = '/home/steamos/frameplayerx/frameplayerx.sh'

    def test_remove_entries_returns_their_app_ids(self):
        doc = [['shortcuts', [['0', other('a')]]]]
        aid, _ = fai.upsert(doc, 'Laterna', self.exe, '/home/steamos/frameplayerx', '')
        self.assertEqual(fai.remove_entries(doc, 'Laterna', self.exe), {aid})
        self.assertEqual(fai.remove_entries(doc, 'Laterna', self.exe), set())
        self.assertEqual(len(fai.get(doc, 'shortcuts')), 1)

    def test_remove_art_deletes_only_those_apps_artwork(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as root:
            grid = os.path.join(root, 'userdata', '1234', 'config', 'grid')
            os.makedirs(grid)
            names = ['3000000001p.png', '3000000001_hero.png', '3000000001_logo.png', '3000000001.png',
                     '3000000002p.png']
            for n in names:
                open(os.path.join(grid, n), 'w').close()
            with patch.object(fai, 'STEAM_ROOTS', [root]):
                fai.remove_art({3000000001, 3000000003})
            self.assertEqual(os.listdir(grid), ['3000000002p.png'])

    def test_data_dirs_follow_xdg(self):
        from unittest.mock import patch

        with patch.dict(os.environ, {'XDG_CONFIG_HOME': '/c', 'XDG_DATA_HOME': '', 'XDG_CACHE_HOME': '/k'}):
            self.assertEqual(fai.data_dirs(), ['/c/frameplayerx',
                                               os.path.join(fai.HOME, '.local/share', 'frameplayerx'),
                                               '/k/frameplayerx'])

    def test_auto_yes_never_deletes_data(self):
        from unittest.mock import patch

        env = {'FRAMEPLAYERX_INSTALL_YES': '1', 'DISPLAY': '', 'WAYLAND_DISPLAY': ''}
        with patch.dict(os.environ, env), patch.object(fai.sys.stdin, 'isatty', return_value=False):
            self.assertTrue(fai.ask('Close Steam?'))
            self.assertFalse(fai.ask('Delete your data?', assume=False))


if __name__ == '__main__':
    unittest.main()


"""python3 -m unittest scripts/test_bump_versions.py"""

import hashlib
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("bump", Path(__file__).with_name("bump-versions.py"))
bump = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bump)

SHA = "4b14c18f7e90a063b61462ea1462a5732c154bf6adad594974fe6487cd98a8de"


class PickKernel(unittest.TestCase):
    def test_point_release_in_the_current_series(self):
        self.assertEqual(bump.pick_kernel("7.3.3", [("7.3.5", False)]), "7.3.5")

    def test_new_series_waits_for_its_second_point_release(self):
        self.assertEqual(bump.pick_kernel("7.3.5", [("7.4.1", False), ("7.3.6", False)]), "7.3.6")
        self.assertEqual(bump.pick_kernel("7.3.5", [("7.4.2", False), ("7.3.6", False)]), "7.4.2")

    def test_end_of_life_series_moves_right_away(self):
        self.assertEqual(bump.pick_kernel("7.3.9", [("7.4.0", False), ("7.3.9", True)]), "7.4.0")
        self.assertEqual(bump.pick_kernel("7.2.15", [("7.4.1", False), ("7.3.3", False)]), "7.4.1")

    def test_a_pinned_series_that_turned_longterm_keeps_its_point_releases(self):
        releases = [{"moniker": "stable", "version": "7.5", "iseol": False}, {"moniker": "longterm", "version": "7.4.10", "iseol": False}]
        self.assertEqual(bump.pick_kernel("7.4.9", bump.candidates(releases, "7.4.9")), "7.4.10")
        releases[0]["version"] = "7.5.2"
        self.assertEqual(bump.pick_kernel("7.4.9", bump.candidates(releases, "7.4.9")), "7.5.2")

    def test_other_longterm_series_are_not_candidates(self):
        releases = [{"moniker": "longterm", "version": "6.18.40", "iseol": False}, {"moniker": "stable", "version": "7.4.3", "iseol": False}]
        self.assertEqual(bump.candidates(releases, "7.4.1"), [("7.4.3", False)])

    def test_never_older_than_the_pin(self):
        self.assertEqual(bump.pick_kernel("7.4.3", []), "7.4.3")
        self.assertEqual(bump.pick_kernel("7.4.3", [("7.3.12", True)]), "7.4.3")
        self.assertEqual(bump.pick_kernel("7.5.1", [("7.3.9", False)]), "7.5.1")


class BaseConfig(unittest.TestCase):
    def test_stale_when_its_series_differs_from_the_pin(self):
        header = "# Base kernel config: Alpine edge linux-virt 7.3.3-r0, /boot/config-7.3.3-0-virt.\n"
        self.assertEqual(bump.config_version(header), "7.3.3-r0")
        self.assertFalse(bump.config_stale(header, "7.3.9"))
        self.assertTrue(bump.config_stale(header, "7.4.2"))
        self.assertTrue(bump.config_stale("CONFIG_X=y\n", "7.4.2"))

    def test_the_vendored_config_matches_the_pinned_kernel(self):
        env = bump.read_env(bump.VERSIONS.read_text())
        self.assertFalse(bump.config_stale(bump.BASE_CONFIG.read_text(), env["LINUX_VERSION"]))


class Files(unittest.TestCase):
    def test_set_env_changes_one_value(self):
        text = ': "${LINUX_VERSION:=7.3.3}"\n: "${LINUX_SHA256:=ab}"\n'
        self.assertEqual(bump.set_env(text, "LINUX_VERSION", "7.3.4"), ': "${LINUX_VERSION:=7.3.4}"\n: "${LINUX_SHA256:=ab}"\n')
        self.assertEqual(bump.read_env(text), {"LINUX_VERSION": "7.3.3", "LINUX_SHA256": "ab"})

    def test_minirootfs_from_latest_releases_yaml(self):
        yaml = f"---\n-\n  flavor: alpine-rpi\n  version: 3.24.4\n  sha256: aa\n-\n  flavor: alpine-minirootfs\n  version: 3.24.4\n  sha256: {SHA}\n"
        self.assertEqual(bump.minirootfs(yaml), ("3.24.4", SHA))

    def test_minirootfs_rejects_what_isnt_a_version_or_a_hash(self):
        for version, sha in [("3.24.4", "bb"), ("3.24.4", SHA.upper()), ("3.25.0$(id)", SHA)]:
            yaml = f"-\n  flavor: alpine-minirootfs\n  version: {version}\n  sha256: {sha}\n"
            with self.assertRaises(SystemExit, msg=(version, sha)):
                bump.minirootfs(yaml)


class Injection(unittest.TestCase):
    """build.sh sources versions.env as shell; cli_fetch.rs is Rust source."""

    TEXT = ': "${ALPINE_RELEASE:=3.24.4}"\n: "${MINIROOTFS_SHA256:=' + "0" * 64 + '}"\n'

    def test_set_env_rejects_shell(self):
        self.assertIn("{ALPINE_RELEASE:=3.25}", bump.set_env(self.TEXT, "ALPINE_RELEASE", "3.25"))
        for value in ["3.25.0$(id)", "3.25.0`id`", '3.24.5}"; echo x; : "${X:=1', "3.25.0\\1"]:
            with self.assertRaises(SystemExit, msg=value):
                bump.set_env(self.TEXT, "ALPINE_RELEASE", value)

    def test_set_env_rejects_a_hash_that_isnt_one(self):
        with self.assertRaises(SystemExit):
            bump.set_env(self.TEXT, "MINIROOTFS_SHA256", "ab")

    def test_replace_asset_rejects_quotes_and_bad_digests(self):
        text = 'url: "https://github.com/docker/compose/releases/download/v2.0.0/x",\n        sha256: "' + "0" * 64 + '"'
        url_re = r"https://github\.com/docker/compose/releases/download/v[^\"]+"
        good = "https://github.com/docker/compose/releases/download/v2.1.0/x"
        self.assertIn(good, bump.replace_asset(text, url_re, good, SHA))
        with self.assertRaises(SystemExit):
            bump.replace_asset(text, url_re, good + '", evil: "', SHA)
        with self.assertRaises(SystemExit):
            bump.replace_asset(text, url_re, good, "not-hex")


class Keys(unittest.TestCase):
    """The committed keyrings are the keys the docstring names (kernel.org, Alpine)."""

    def fingerprint(self, name):
        packet = bump.dearmor((bump.KEYS / name).read_text())
        # Old-format public key packet with a 2-byte length: fingerprint = SHA-1(0x99, len, body).
        self.assertEqual(packet[0], 0x99)
        n = int.from_bytes(packet[1:3], "big")
        return hashlib.sha1(b"\x99" + packet[1 : 3 + n]).hexdigest().upper()

    def test_fingerprints(self):
        self.assertEqual(self.fingerprint("kernel.org-autosigner.asc"), "B8868C80BA62A1FFFAF5FDA9632D3A06589DA6B1")
        self.assertEqual(self.fingerprint("alpine-ncopa.asc"), "0482D84022F52DF1C4E7CD43293ACD0907D9495A")


if __name__ == "__main__":
    unittest.main()

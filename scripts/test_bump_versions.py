"""python3 -m unittest scripts/test_bump_versions.py"""

import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("bump", Path(__file__).with_name("bump-versions.py"))
bump = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bump)


class PickKernel(unittest.TestCase):
    def test_point_release_in_the_current_series(self):
        self.assertEqual(bump.pick_kernel("7.3.3", [("7.3.5", False)]), "7.3.5")

    def test_new_series_waits_for_its_second_point_release(self):
        self.assertEqual(bump.pick_kernel("7.3.5", [("7.4.1", False), ("7.3.6", False)]), "7.3.6")
        self.assertEqual(bump.pick_kernel("7.3.5", [("7.4.2", False), ("7.3.6", False)]), "7.4.2")

    def test_end_of_life_series_moves_right_away(self):
        self.assertEqual(bump.pick_kernel("7.3.9", [("7.4.0", False), ("7.3.9", True)]), "7.4.0")
        self.assertEqual(bump.pick_kernel("7.2.15", [("7.4.1", False), ("7.3.3", False)]), "7.4.1")


class Files(unittest.TestCase):
    def test_set_env_changes_one_value(self):
        text = ': "${LINUX_VERSION:=7.3.3}"\n: "${LINUX_SHA256:=ab}"\n'
        self.assertEqual(bump.set_env(text, "LINUX_VERSION", "7.3.4"), ': "${LINUX_VERSION:=7.3.4}"\n: "${LINUX_SHA256:=ab}"\n')
        self.assertEqual(bump.read_env(text), {"LINUX_VERSION": "7.3.3", "LINUX_SHA256": "ab"})

    def test_minirootfs_from_latest_releases_yaml(self):
        yaml = "---\n-\n  flavor: alpine-rpi\n  version: 3.24.4\n  sha256: aa\n-\n  flavor: alpine-minirootfs\n  version: 3.24.4\n  sha256: bb\n"
        self.assertEqual(bump.minirootfs(yaml), ("3.24.4", "bb"))


if __name__ == "__main__":
    unittest.main()

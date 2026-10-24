import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("formula", Path(__file__).with_name("homebrew-formula.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class Formula(unittest.TestCase):
    def checksums(self):
        return "\n".join(f"{'0' * 64}  hyperag-v0.2.0-{target}.tar.gz" for target in module.TARGETS)

    def test_four_platforms_are_pinned_to_release_checksums(self):
        text = module.formula("v0.2.0", self.checksums())
        self.assertEqual(text.count('sha256 "'), 4)
        self.assertIn('version "0.2.0"', text)
        self.assertIn('hyperag --version', text)
        self.assertTrue(all(target in text for target in module.TARGETS))

    def test_missing_duplicate_and_invalid_metadata_fail_closed(self):
        for version, checksums in [("../../bad", self.checksums()), ("v0.2.0", ""),
                                   ("v0.2.0", self.checksums() + "\n" + self.checksums()), ("v0.2.0", "not a checksum")]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                module.formula(version, checksums)


if __name__ == "__main__":
    unittest.main()

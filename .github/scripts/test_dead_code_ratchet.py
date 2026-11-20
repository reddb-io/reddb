import importlib.util
import pathlib
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "dead_code_ratchet", pathlib.Path(__file__).with_name("dead-code-ratchet.py")
)
ratchet = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ratchet)


class DeadCodeRatchet(unittest.TestCase):
    def test_counts_suppressions_and_deprecations_in_rust_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "crates" / "a" / "src").mkdir(parents=True)
            (root / "crates" / "a" / "target").mkdir()
            (root / "crates" / "a" / "src" / "lib.rs").write_text(
                "#[allow(dead_code)]\nfn a() {}\n"
                "#![allow(unused, dead_code)]\n"
                '#[deprecated(note = "x")]\nfn b() {}\n'
                "// not a suppression: dead code\n"
            )
            (root / "crates" / "a" / "target" / "gen.rs").write_text("#[allow(dead_code)]\n")
            (root / "docs").mkdir()
            (root / "docs" / "note.rs").write_text("#[allow(dead_code)]\n")
            self.assertEqual(ratchet.count(root), {"allow_dead_code": 2, "deprecated": 1})

    def test_only_an_exact_baseline_passes(self):
        baseline = {"allow_dead_code": 2, "deprecated": 1}
        self.assertEqual(ratchet.verdict(dict(baseline), baseline), [])
        grew = ratchet.verdict({"allow_dead_code": 3, "deprecated": 1}, baseline)
        self.assertEqual(len(grew), 1)
        self.assertIn("Remove the dead code", grew[0])
        shrank = ratchet.verdict({"allow_dead_code": 2, "deprecated": 0}, baseline)
        self.assertEqual(len(shrank), 1)
        self.assertIn("Lower", shrank[0])


if __name__ == "__main__":
    unittest.main()

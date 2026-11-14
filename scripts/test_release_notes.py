import importlib.util
import pathlib
import unittest

spec = importlib.util.spec_from_file_location(
    "release_notes", pathlib.Path(__file__).with_name("release-notes.py")
)
notes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notes)

R = "https://github.com/userfypp/phasewise"
SHA = "a" * 40
CHANGELOG = f"""# Changelog

## [0.3.0]({R}/compare/v0.2.0...v0.3.0) (2026-12-01)


### ⚠ BREAKING CHANGES

* **plan:** rename the `model` key to `agent_model`

### Added

* **tui:** show the cost of a run ([#12]({R}/issues/12)) ([abcdef1]({R}/commit/{SHA})), closes [#7]({R}/issues/7)
* `phasewise models` lists what an agent offers ([abcdef2]({R}/commit/{SHA}))

### Fixed

* **git:** retry when index.lock is taken
  and keep the note ([#13]({R}/issues/13)) ([abcdef3]({R}/commit/{SHA}))

## [0.2.0]({R}/compare/v0.1.0...v0.2.0) (2026-11-20)

### Added

* older entry
"""


class ReleaseNotes(unittest.TestCase):
    def render(self, version="0.3.0", run_url=None):
        entries, compare = notes.parse(CHANGELOG, version)
        return notes.render(entries, version, compare, run_url=run_url)

    def test_capital_and_stop(self):
        self.assertEqual(notes.sentence("show the cost"), "Show the cost.")
        self.assertEqual(notes.sentence("Show the cost."), "Show the cost.")
        self.assertEqual(notes.sentence("show it?"), "Show it?")

    def test_own_spelling_is_kept(self):
        self.assertEqual(notes.sentence("`phasewise models` lists"), "`phasewise models` lists.")
        self.assertEqual(notes.sentence("iOS builds"), "iOS builds.")
        self.assertEqual(notes.sentence("v2 plans load"), "v2 plans load.")

    def test_sections_and_links(self):
        text = self.render()
        self.assertIn("## Breaking changes\n\n- Rename the `model` key to `agent_model`.\n", text)
        self.assertIn(f"- Show the cost of a run. ([#12]({R}/issues/12), [#7]({R}/issues/7))", text)
        self.assertIn(f"- `phasewise models` lists what an agent offers. ([abcdef2]({R}/commit/{SHA}))", text)
        self.assertLess(text.index("## Breaking changes"), text.index("## Added"))
        self.assertLess(text.index("## Added"), text.index("## Fixed"))

    def test_continuation_lines_join(self):
        self.assertIn("- Retry when index.lock is taken and keep the note.", self.render())

    def test_other_versions_are_left_out(self):
        self.assertNotIn("Older entry", self.render())
        self.assertIn("Older entry.", self.render("0.2.0"))

    def test_install_and_validation(self):
        text = self.render(run_url=f"{R}/actions/runs/1/attempts/1")
        self.assertIn("cargo install phasewise --version 0.3.0", text)
        self.assertIn("[Workflow run]", text)
        self.assertIn("crates.io/crates/phasewise/0.3.0", text)
        self.assertTrue(text.endswith(f"Full changelog: [v0.2.0...v0.3.0]({R}/compare/v0.2.0...v0.3.0).\n"))
        self.assertNotIn("Workflow run", self.render())

    def test_errors(self):
        with self.assertRaises(notes.NotesError):
            notes.parse(CHANGELOG, "9.9.9")
        with self.assertRaises(notes.NotesError):
            notes.parse(CHANGELOG, "1.0")
        with self.assertRaises(notes.NotesError):
            notes.parse("## [1.0.0] (x)\n\n### Odd\n\n* x\n", "1.0.0")


if __name__ == "__main__":
    unittest.main()

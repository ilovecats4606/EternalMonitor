"""check_messages.py must reject a troubleshooting quote the apps don't show."""
import pathlib
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import check_messages  # noqa: E402

RUST = '''
const TITLE: &str = "The update did not finish";
fn banner(version: &str) -> String { format!("EternalMonitor {version} is available") }
'''
SWIFT = '''
let a = "That code didn’t match. Check the PC and try again."
let b = "Reaching \\(normalizedHostIP)"
'''
SETUP = '''# Set up

## If something goes wrong

### Messages on the iPad

| What the iPad says | What it means | What to do |
| --- | --- | --- |
| {ipad} | It means this. | Do that. |

### Messages on the PC

| What the PC window says | What it means | What to do |
| --- | --- | --- |
| **EternalMonitor … is available** | A new version. | Click **Update now**. |

### Messages from Windows, and problems without a message

| What you see | What it means | What to do |
| --- | --- | --- |
| **Windows protected your PC** | Not from the apps. | Click **Run anyway**. |
'''
QUICKSTART = '''Quick Start

If something goes wrong
-----------------------
- "{quick}": do this,
  over two lines.
- "The update did not finish": try again.

Getting help
------------
- "Not checked here": fine.
'''


class CheckMessagesTest(unittest.TestCase):
    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp())
        for name, text in (("host/src/gui/a.rs", RUST), ("ios/EternalMonitor/App/b.swift", SWIFT)):
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_text(text, encoding="utf-8")
        (self.root / "scripts").mkdir()

    def tearDown(self):
        shutil.rmtree(self.root)

    def problems(self, ipad="**That code didn’t match. Check the PC and try again.**",
                 quick="That code didn't match. Check the PC and try again."):
        (self.root / "SETUP.md").write_text(SETUP.format(ipad=ipad), encoding="utf-8")
        (self.root / "scripts/QUICKSTART.txt").write_text(QUICKSTART.format(quick=quick), encoding="utf-8")
        return check_messages.check(self.root)

    def test_exact_quotes_pass(self):
        self.assertEqual(self.problems(), [])

    def test_shortened_quote_fails(self):
        problems = self.problems(ipad="**That code didn’t match**")
        self.assertEqual(len(problems), 1)
        self.assertIn("SETUP.md:9: not an exact app message: That code didn’t match", problems[0])

    def test_setup_keeps_the_apps_curly_apostrophe(self):
        self.assertEqual(len(self.problems(ipad="**That code didn't match. Check the PC and try again.**")), 1)

    def test_placeholders_are_written_as_an_ellipsis(self):
        self.assertEqual(self.problems(ipad="**Reaching …**"), [])
        self.assertEqual(len(self.problems(ipad="**Reaching 192.168.1.20**")), 1)

    def test_quickstart_is_checked_in_ascii(self):
        self.assertEqual(len(self.problems(quick="That code didn't match")), 1)
        problems = self.problems(quick="That code didn’t match. Check the PC and try again.")
        self.assertIn("scripts/QUICKSTART.txt: must be ASCII", problems)

    def test_the_repository_quotes_match_the_apps(self):
        self.assertEqual(check_messages.check(), [])


if __name__ == "__main__":
    unittest.main()

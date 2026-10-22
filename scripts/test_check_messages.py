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
let c = "No PCs found."
let d = "No PCs found. Check that EternalMonitor is open on the PC."
let e = "Check that EternalMonitor is open on the PC, "
    + "then try again."
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

    def test_first_sentence_of_a_longer_message_fails(self):
        # "No PCs found." is a message of its own, but the row means the
        # longer one, so a quote cut down to its first sentence must fail.
        problems = self.problems(ipad="**No PCs found.**")
        self.assertEqual(len(problems), 1)
        self.assertIn("only the start of the app message", problems[0])
        self.assertEqual(self.problems(ipad="**No PCs found. Check that EternalMonitor is open on the PC.**"), [])

    def test_a_title_that_is_not_a_sentence_may_start_a_longer_message(self):
        # "The update did not finish" and "…did not finish, so …" are both whole.
        (self.root / "host/src/gui/c.rs").write_text('const B: &str = "The update did not finish, so try again.";')
        self.assertEqual(self.problems(), [])

    def test_joined_literals_are_one_message(self):
        self.assertEqual(self.problems(ipad="**Check that EternalMonitor is open on the PC, then try again.**"), [])
        self.assertEqual(len(self.problems(ipad="**Check that EternalMonitor is open on the PC,**")), 1)

    def test_the_repository_quotes_match_the_apps(self):
        self.assertEqual(check_messages.check(), [])



def rust(source):
    return {check_messages._spaces(m) for m in check_messages.rust_literals(source)}


def swift(source):
    return {check_messages._spaces(m) for m in check_messages.swift_literals(source)}


class LiteralFormsTest(unittest.TestCase):
    def test_rust_line_continuation_drops_the_newline_and_indent(self):
        source = 'notice(\n    "Windows didn\'t add the second screen in time. \\\n     Click Try again.",\n)'
        self.assertIn("Windows didn't add the second screen in time. Click Try again.", rust(source))

    def test_rust_literal_spanning_lines_keeps_its_words(self):
        self.assertIn("First line. Second line.", rust('let s = "First line.\n        Second line.";'))

    def test_rust_format_braces(self):
        messages = rust('let a = format!("Use {{braces}} for {name}");\nwrite!(f, "{} of {:.0}", a, b);\nlet b = "{{kept}}";')
        self.assertIn("Use {braces} for …", messages)
        self.assertIn("… of …", messages)
        self.assertIn("{{kept}}", messages)

    def test_rust_raw_strings_chars_comments_and_concat(self):
        messages = rust('''
            let q = '"';
            // "Commented out message"
            /* "Also /* nested */ commented out" */
            let raw = r#"He said "go""#;
            let joined = concat!("Part one, ", "part two.");
            let after = "After a quote char";
        ''')
        self.assertEqual(messages, {'He said "go"', "Part one, part two.", "After a quote char"})

    def test_swift_multiline_strings_follow_the_closing_indent(self):
        source = 'let m = """\n        Line one \\\n        continues.\n          Indented line\n        """\n'
        self.assertEqual(swift(source), {"Line one continues. Indented line"})
        self.assertEqual(check_messages.swift_literals(source), ["Line one continues.\n  Indented line"])

    def test_swift_interpolation_with_nested_strings(self):
        messages = swift('let s = "Found \\(n) PC\\(n == 1 ? "" : "s")"\nlet t = "Next"')
        self.assertEqual(messages, {"Found … PC…", "Next"})

    def test_swift_raw_strings_and_concatenation(self):
        messages = swift('let r = #"Say "hi" \\#(name)"#\nlet j = "One, "\n    + "two."\n// "Not this"')
        self.assertEqual(messages, {'Say "hi" …', "One, two."})


if __name__ == "__main__":
    unittest.main()

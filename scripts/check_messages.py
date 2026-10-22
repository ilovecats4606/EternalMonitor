#!/usr/bin/env python3
"""Check that the troubleshooting guides quote the apps' messages exactly.

SETUP.md's iPad and PC tables put each message in bold in the first column,
and QUICKSTART.txt lists them as `- "message": what to do`. Each quote must
equal a whole string in the Windows host or iPad app source. A placeholder
such as {version} or \\(host) in the source is written as an ellipsis (…).
QUICKSTART.txt is ASCII, so there it is "..." and apostrophes are straight.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCES = [("host/src", "*.rs"), ("ios/EternalMonitor", "*.swift")]
CHECKED_TABLES = ("### Messages on the iPad", "### Messages on the PC")
LITERAL = re.compile(r'"((?:[^"\\\n]|\\.)*)"')
PLACEHOLDER = re.compile(r"\{[^{}]*\}|\\\([^()]*(?:\([^()]*\)[^()]*)*\)")


def source_messages(root):
    """Every string literal in the app sources, with placeholders as '…'."""
    messages = set()
    for folder, pattern in SOURCES:
        for path in sorted((root / folder).rglob(pattern)):
            text = re.sub(r"\\\n\s*", "", path.read_text(encoding="utf-8"))
            for literal in LITERAL.findall(text):
                literal = PLACEHOLDER.sub("…", literal).replace('\\"', '"')
                messages.add(literal)
    return messages


def setup_quotes(text):
    """Bold quotes in the first column of the iPad and PC tables."""
    quotes = []
    table = None
    for number, line in enumerate(text.splitlines(), 1):
        if line.startswith("#"):
            table = line.strip() if line.strip() in CHECKED_TABLES else None
        elif table and line.startswith("| ") and not line.startswith("| ---"):
            first = line.split(" | ")[0]
            if first.startswith("| What "):
                continue
            quotes += [(number, quote) for quote in re.findall(r"\*\*(.+?)\*\*", first)]
    return quotes


def quickstart_quotes(text):
    """Quoted messages in QUICKSTART.txt's "If something goes wrong" section."""
    section = text.split("If something goes wrong\n", 1)[1].split("\nGetting help\n", 1)[0]
    quotes = []
    for item in re.split(r"\n- ", section)[1:]:
        match = re.match(r'"(.+?)":', " ".join(item.split()), re.S)
        if match:
            line = text.count("\n", 0, text.index(item)) + 1
            quotes.append((line, match[1]))
    return quotes


def ascii_form(message):
    return message.replace("’", "'").replace("…", "...")


def check(root=ROOT):
    """The list of problems; empty when every quote matches."""
    messages = source_messages(root)
    problems = []
    setup = setup_quotes((root / "SETUP.md").read_text(encoding="utf-8"))
    if not setup:
        problems.append("SETUP.md: no quoted messages found in the iPad and PC tables")
    for line, quote in setup:
        if quote not in messages:
            problems.append(f"SETUP.md:{line}: not an exact app message: {quote}")
    ascii_messages = {ascii_form(m) for m in messages}
    quickstart = (root / "scripts/QUICKSTART.txt").read_text(encoding="utf-8")
    if not quickstart.isascii():
        problems.append("scripts/QUICKSTART.txt: must be ASCII")
    found = quickstart_quotes(quickstart)
    if not found:
        problems.append("scripts/QUICKSTART.txt: no quoted messages found")
    for line, quote in found:
        if quote not in ascii_messages:
            problems.append(f"scripts/QUICKSTART.txt:{line}: not an exact app message: {quote}")
    return problems


def main():
    problems = check()
    for problem in problems:
        print(f"FAIL: {problem}")
    if problems:
        sys.exit(1)
    print("Troubleshooting messages match the apps")


if __name__ == "__main__":
    main()

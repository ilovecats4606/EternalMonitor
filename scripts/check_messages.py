#!/usr/bin/env python3
"""Check that the troubleshooting guides quote the apps' messages exactly.

SETUP.md's iPad and PC tables put each message in bold in the first column,
and QUICKSTART.txt lists them as `- "message": what to do`. Each quote must
equal a whole message in the Windows host or iPad app source: one string
literal as the app renders it, or literals the source joins into one message
(Swift `+`, Rust `concat!`). A quote that ends a sentence where a longer
message goes on with another one fails too. A placeholder such as {version} or \\(host) in the
source is written as an ellipsis (…). Runs of whitespace compare as one space.
QUICKSTART.txt is ASCII, so there it is "..." and apostrophes are straight.
"""
import pathlib
import re
import sys
from typing import NamedTuple

ROOT = pathlib.Path(__file__).resolve().parents[1]
CHECKED_TABLES = ("### Messages on the iPad", "### Messages on the PC")
ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
RUST_RAW = re.compile(r'b?r(#*)"')
RUST_CHAR = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'")
# The format string is a macro's first argument, or its second after a writer.
RUST_FORMAT_CALL = re.compile(r"\w!\(\s*(?:[^,()\"]+,\s*)?$")
SWIFT_OPEN = re.compile(r'(#*)("""|")')


class Literal(NamedTuple):
    value: str
    start: int
    end: int


def _identifier_before(text, i):
    return i > 0 and (text[i - 1].isalnum() or text[i - 1] == "_")


def _skip_comment(text, i):
    """The index after a // or (nested) /* */ comment starting at i, or None."""
    if text.startswith("//", i):
        end = text.find("\n", i)
        return len(text) if end < 0 else end
    if not text.startswith("/*", i):
        return None
    depth, i = 1, i + 2
    while depth:
        if text.startswith("/*", i):
            depth, i = depth + 1, i + 2
        elif text.startswith("*/", i):
            depth, i = depth - 1, i + 2
        else:
            i += 1
    return i


def _escape(text, i, skip_indent):
    """Decode the escape whose backslash is at i: (text, next index)."""
    c = text[i + 1]
    if c in ESCAPES:
        return ESCAPES[c], i + 2
    if c == "u":
        close = text.index("}", i)
        return chr(int(text[i + 3:close], 16)), close + 1
    if c == "x":
        return chr(int(text[i + 2:i + 4], 16)), i + 4
    if c == "\n":
        # A backslash ending a line joins the next line. Rust also drops that
        # line's leading whitespace; Swift's indentation is already removed.
        i += 2
        while skip_indent and i < len(text) and text[i].isspace():
            i += 1
        return "", i
    return c, i + 2


def _rust_format(value):
    """A format string as printed: {{ and }} are braces, {…} is a value."""
    value = value.replace("{{", "\x01").replace("}}", "\x02")
    value = re.sub(r"\{[^{}]*\}", "…", value)
    return value.replace("\x01", "{").replace("\x02", "}")


def rust_literals(text):
    literals, i = [], 0
    while i < len(text):
        after_comment = _skip_comment(text, i)
        raw = RUST_RAW.match(text, i)
        if after_comment is not None:
            i = after_comment
        elif raw and not _identifier_before(text, i):
            close = text.index('"' + raw[1], raw.end())
            literals.append(Literal(text[raw.end():close], i, close + 1 + len(raw[1])))
            i = close + 1 + len(raw[1])
        elif text[i] == '"' or (text.startswith('b"', i) and not _identifier_before(text, i)):
            start, i, out = i, text.index('"', i) + 1, []
            while text[i] != '"':
                if text[i] == "\\":
                    piece, i = _escape(text, i, skip_indent=True)
                else:
                    piece, i = text[i], i + 1
                out.append(piece)
            value = "".join(out)
            if RUST_FORMAT_CALL.search(text[max(0, start - 120):start]):
                value = _rust_format(value)
            literals.append(Literal(value, start, i + 1))
            i += 1
        elif text[i] == "'" and RUST_CHAR.match(text, i):
            i = RUST_CHAR.match(text, i).end()
        else:
            i += 1
    return _join(text, literals, r"\s*,\s*", opener=r"concat!\(\s*$")


def _skip_parens(text, i):
    """The index after the balanced parentheses starting at i."""
    depth = 0
    while True:
        if SWIFT_OPEN.match(text, i) and text[i] in '"#':
            _, i = swift_string(text, i)
            continue
        depth += {"(": 1, ")": -1}.get(text[i], 0)
        i += 1
        if depth == 0:
            return i


def _swift_body(text, i, close, escape):
    out = []
    while not text.startswith(close, i):
        if text.startswith(escape + "(", i):
            i = _skip_parens(text, i + len(escape))
            out.append("…")
        elif text.startswith(escape, i):
            piece, i = _escape(text[:i] + "\\" + text[i + len(escape):], i, skip_indent=False)
            i += len(escape) - 1
            out.append(piece)
        else:
            out.append(text[i])
            i += 1
    return "".join(out), i + len(close)


def swift_string(text, i):
    """(rendered value, end) for the Swift string literal starting at i."""
    opening = SWIFT_OPEN.match(text, i)
    hashes, quote = opening[1], opening[2]
    escape = "\\" + hashes
    if quote == '"':
        return _swift_body(text, opening.end(), '"' + hashes, escape)
    # """: content starts on the next line and ends before the closing line,
    # whose indentation is removed from every line.
    begin = text.index("\n", opening.end()) + 1
    close = text.index('"""' + hashes, begin)
    last_newline = text.rindex("\n", begin - 1, close)
    indent = text[last_newline + 1:close]
    lines = [line[len(indent):] if line.startswith(indent) else line.lstrip()
             for line in text[begin:last_newline].split("\n")] if last_newline >= begin else []
    value, _ = _swift_body("\n".join(lines) + '"""', 0, '"""', escape)
    return value, close + 3 + len(hashes)


def swift_literals(text):
    literals, i = [], 0
    while i < len(text):
        after_comment = _skip_comment(text, i)
        if after_comment is not None:
            i = after_comment
        elif SWIFT_OPEN.match(text, i) and (text[i] == '"' or not _identifier_before(text, i)):
            value, end = swift_string(text, i)
            literals.append(Literal(value, i, end))
            i = end
        else:
            i += 1
    return _join(text, literals, r"\s*\+\s*")


def _join(text, literals, joiner, opener=None):
    """Messages, with literals the source joins into one message merged."""
    messages = []
    for literal in literals:
        previous = messages[-1] if messages else None
        joined = previous and re.fullmatch(joiner, text[previous[1]:literal.start])
        if joined and (opener is None or re.search(opener, text[:previous[2]])):
            messages[-1] = (previous[0] + literal.value, literal.end, previous[2])
        else:
            messages.append((literal.value, literal.end, literal.start))
    return [value for value, _, _ in messages]


SOURCES = [("host/src", "*.rs", rust_literals), ("ios/EternalMonitor", "*.swift", swift_literals)]


def _spaces(text):
    return " ".join(text.split())


def source_messages(root):
    """Every whole message in the app sources, as the user would read it."""
    messages = set()
    for folder, pattern, extract in SOURCES:
        for path in sorted((root / folder).rglob(pattern)):
            messages.update(_spaces(m) for m in extract(path.read_text(encoding="utf-8")))
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


def _problem(quote, messages):
    """Why a quote is not a whole app message, or None."""
    if quote not in messages:
        return "not an exact app message"
    longer = sorted(m for m in messages if m.startswith(quote + " "))
    if longer and quote[-1] in ".!?":
        return f"only the start of the app message \"{longer[0]}\""
    return None


def check(root=ROOT):
    """The list of problems; empty when every quote matches."""
    messages = source_messages(root)
    problems = []
    setup = setup_quotes((root / "SETUP.md").read_text(encoding="utf-8"))
    if not setup:
        problems.append("SETUP.md: no quoted messages found in the iPad and PC tables")
    for line, quote in setup:
        problem = _problem(_spaces(quote), messages)
        if problem:
            problems.append(f"SETUP.md:{line}: {problem}: {quote}")
    ascii_messages = {ascii_form(m) for m in messages}
    quickstart = (root / "scripts/QUICKSTART.txt").read_text(encoding="utf-8")
    if not quickstart.isascii():
        problems.append("scripts/QUICKSTART.txt: must be ASCII")
    found = quickstart_quotes(quickstart)
    if not found:
        problems.append("scripts/QUICKSTART.txt: no quoted messages found")
    for line, quote in found:
        problem = _problem(_spaces(quote), ascii_messages)
        if problem:
            problems.append(f"scripts/QUICKSTART.txt:{line}: {problem}: {quote}")
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

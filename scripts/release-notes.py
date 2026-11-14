#!/usr/bin/env python3
"""Turns the changelog section of one version into the notes of its GitHub
Release.

Release Please writes CHANGELOG.md from the titles of the merged pull requests:
lower case, no final stop, one link to the commit and one to the pull request.
The notes are the same entries, normalised: a capital first letter, a final
stop, links to the pull request and the issues it closes (or to the commit when
there is none), the scope dropped, and the sections in a fixed order. When a
workflow run is given, the notes end with what that run verified.

Usage:
    python3 scripts/release-notes.py --version X.Y.Z [--changelog CHANGELOG.md]
        [--run-url URL] [--output FILE]

Needs only the standard library.
"""

import argparse
import re
import sys

# Sections of the notes, in order, and the changelog headings that feed them.
SECTIONS = [
    ("Breaking changes", ("⚠ BREAKING CHANGES", "BREAKING CHANGES")),
    ("Added", ("Added",)),
    ("Improved", ("Improved",)),
    ("Fixed", ("Fixed",)),
    ("Reverted", ("Reverted",)),
    ("Documentation", ("Documentation",)),
]
VERSION = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
LINK = r"\[([^\]]+)\]\((https://github\.com/[\w.-]+/[\w.-]+/(?:issues|pull|commit)/[\w]+)\)"
TRAILING_LINK = re.compile(r"\s*\(" + LINK + r"\)\s*$")
CLOSES_FOOTER = re.compile(r",\s*closes\s+((?:" + LINK + r"[,\s]*)+)$", re.IGNORECASE)
SCOPE = re.compile(r"^\*\*[^*]+:\*\*\s+")
COMPARE = re.compile(r"\]\((https://github\.com/[^)\s]+/compare/[^)\s]+)\)")


class NotesError(Exception):
    pass


def split_entry(raw):
    """Returns the text of an entry and its links: ([(label, url)], commit)."""
    text = SCOPE.sub("", raw.strip())
    references, commit = [], None
    closes = CLOSES_FOOTER.search(text)
    if closes:
        for label, url in re.findall(LINK, closes.group(1)):
            references.append((label, url))
        text = text[: closes.start()].rstrip()
    while True:
        match = TRAILING_LINK.search(text)
        if not match:
            break
        label, url = match.groups()
        if "/commit/" in url:
            commit = (label, url)
        else:
            references.insert(0, (label, url))
        text = text[: match.start()].rstrip()
    if not text:
        raise NotesError("a changelog entry is empty once its links are removed")
    return text, references, commit


def parse(changelog, version):
    """Returns the entries of one version as (section, text, links) and the
    compare link of its heading, if it has one."""
    if not VERSION.match(version):
        raise NotesError(f'expected a version like 1.2.3, got "{version}"')
    lines = changelog.replace("\r\n", "\n").split("\n")
    heading = re.compile(r"^## \[?" + re.escape(version) + r"\]?(?:\(|\s|$)")
    starts = [i for i, line in enumerate(lines) if heading.match(line)]
    if len(starts) != 1:
        raise NotesError(
            f"version {version} must appear once in the changelog, found {len(starts)}"
        )
    compare = COMPARE.search(lines[starts[0]])
    known = {name for _, names in SECTIONS for name in names}
    entries, section = [], None
    for line in lines[starts[0] + 1 :]:
        if line.startswith("## "):
            break
        if not line.strip():
            continue
        title = re.match(r"^### (.+?)\s*$", line)
        if title:
            section = title.group(1)
            if section not in known:
                raise NotesError(f"unknown changelog section: {section}")
            continue
        bullet = re.match(r"^[-*] (.+)$", line)
        if section and bullet:
            entries.append([section, bullet.group(1)])
        elif section and entries and re.match(r"^ {2,}\S", line):
            entries[-1][1] += " " + line.strip()
        else:
            raise NotesError(f"unexpected changelog content: {line}")
    if not entries:
        raise NotesError(f"version {version} has no entries")
    return [(s, *split_entry(raw)) for s, raw in entries], compare.group(1) if compare else None


def sentence(text):
    """A capital first letter and a final stop. A word that already has its own
    spelling (`phasewise`, iOS, v2, a path) keeps it."""
    text = text.strip()
    first = text.split()[0]
    if first.isalpha() and first.islower():
        text = text[0].upper() + text[1:]
    if text[-1] not in ".!?":
        text += "."
    return text


def render(entries, version, compare=None, repository=None, run_url=None):
    groups = {title: [] for title, _ in SECTIONS}
    for section, text, references, commit in entries:
        title = next(t for t, names in SECTIONS if section in names)
        links = list(dict.fromkeys(f"[{label}]({url})" for label, url in references))
        if not links and commit:
            links.append(f"[{commit[0][:7]}]({commit[1]})")
        suffix = f" ({', '.join(links)})" if links else ""
        groups[title].append(f"- {sentence(text)}{suffix}")
    blocks = [f"## {title}\n\n" + "\n".join(items) for title, items in groups.items() if items]
    blocks.append(f"## Install\n\n```sh\ncargo install phasewise --version {version}\n```")
    checks = []
    if run_url:
        checks.append(
            "- Formatting, lints and tests passed on Linux and macOS, and the crate builds "
            "with the oldest supported Rust."
        )
        checks.append(f"- Published to [crates.io](https://crates.io/crates/phasewise/{version}).")
        checks.append(f"- [Workflow run]({run_url}).")
    if checks:
        blocks.append("## Validation\n\n" + "\n".join(checks))
    if compare:
        blocks.append(f"Full changelog: [{compare.rsplit('/', 1)[-1]}]({compare}).")
    return "\n\n".join(blocks) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--version", required=True)
    parser.add_argument("--changelog", default="CHANGELOG.md")
    parser.add_argument("--run-url")
    parser.add_argument("--output")
    args = parser.parse_args()
    try:
        with open(args.changelog, encoding="utf-8") as handle:
            entries, compare = parse(handle.read(), args.version)
        notes = render(entries, args.version, compare, run_url=args.run_url)
    except (OSError, NotesError) as error:
        sys.exit(f"release-notes: {error}")
    if args.output:
        with open(args.output, "w", encoding="utf-8") as handle:
            handle.write(notes)
    else:
        sys.stdout.write(notes)


if __name__ == "__main__":
    main()

"""Print one version's CHANGELOG.md section, for its GitHub release.

Usage: python3 scripts/release_notes.py VERSION [--changelog PATH]

VERSION may carry a leading "v" (a tag name). Exit status is 1 when the
changelog has no section for VERSION and 2 on usage or I/O errors.
"""

import argparse
import re
import sys
from pathlib import Path

HEADING = re.compile(r"^## \[(?P<version>[^\]]+)\]")
LINK_DEFINITION = re.compile(r"^\[[^\]]+\]: \S+")


def extract_section(text, version):
    """Return the body of a version's section in a Keep a Changelog file.

    Args:
        text: The changelog contents.
        version: The version, with or without a leading "v".

    Returns:
        The section body without its heading, surrounding blank lines or
        trailing link definitions, or None when there is no such section.
    """
    wanted = version.removeprefix("v")
    lines = text.splitlines()
    start = None
    end = len(lines)
    for index, line in enumerate(lines):
        match = HEADING.match(line)
        if not match:
            continue
        if start is not None:
            end = index
            break
        if match.group("version") == wanted:
            start = index + 1
    if start is None:
        return None
    body = lines[start:end]
    while body and (not body[-1].strip() or LINK_DEFINITION.match(body[-1])):
        body.pop()
    return "\n".join(body).strip()


def main(argv=None):
    """Print the section, or report why it cannot.

    Args:
        argv: Command-line arguments without the program name.

    Returns:
        The process exit status.
    """
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version")
    parser.add_argument("--changelog", type=Path, default=Path("CHANGELOG.md"))
    args = parser.parse_args(argv)
    try:
        text = args.changelog.read_text(encoding="utf-8")
    except OSError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    section = extract_section(text, args.version)
    if not section:
        print(
            f"error: {args.changelog} has no section for {args.version}; "
            "move [Unreleased] under it before tagging",
            file=sys.stderr,
        )
        return 1
    print(section)
    return 0


if __name__ == "__main__":
    sys.exit(main())

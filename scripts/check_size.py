#!/usr/bin/env python3
"""Size guard for Rust sources (roadmap R09).

Rules (see scripts/size_baseline.json and CONTRIBUTING.md):

* A file must stay below MAX_FILE_LINES (1,000) lines. Exceptions are
  recorded per file and may only shrink.
* No line may exceed HARD_WIDTH (100) columns, which is rustfmt's
  max_width. The count of such lines is a ratchet per package (the
  crates/<name>, examples/<name>, benchmarks/<name> directory, or the
  top-level directory) and may not grow.
* The count of lines above SOFT_WIDTH (79) is also a per-package ratchet
  and may not grow, so new code stays at 79 columns while a file split
  inside a package can move existing long lines freely.

Packages absent from the baseline have a budget of zero. Width counts
East Asian wide characters as two columns. Usage:

    python3 scripts/check_size.py            # check
    python3 scripts/check_size.py --strict   # also fail on stale entries
    python3 scripts/check_size.py --update   # ratchet baseline down only
    python3 scripts/check_size.py --init     # create baseline if missing

Exit status: 0 ok, 1 violations, 2 usage or environment error.
"""

import argparse
import json
import os
import subprocess
import sys
import unicodedata

VERSION = 2
MAX_FILE_LINES = 1000  # files must be strictly below this
HARD_WIDTH = 100
SOFT_WIDTH = 79
SKIP_TOP = {"target", ".git"}
PACKAGE_ROOTS = {"crates", "examples", "benchmarks"}
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASELINE = os.path.join(ROOT, "scripts", "size_baseline.json")
KEYS = ("oversize_files", "lines_over_100", "lines_over_79")
MESSAGES = {
    "oversize_files": "lines (limit < %d)" % MAX_FILE_LINES,
    "lines_over_100": "lines wider than %d columns" % HARD_WIDTH,
    "lines_over_79": "lines wider than %d columns" % SOFT_WIDTH,
}


class GuardError(Exception):
    """Environment or data problem; reported as one line, exit 2."""


def package_of(rel):
    """Aggregation key: crates/<name>, examples/<name>, or top dir."""
    parts = rel.split("/")
    if len(parts) > 2 and parts[0] in PACKAGE_ROOTS:
        return "/".join(parts[:2])
    return parts[0] if len(parts) > 1 else "."


def width(line):
    if line.isascii():
        return len(line)
    return sum(
        2 if unicodedata.east_asian_width(c) in ("W", "F") else 1
        for c in line
    )


def list_files():
    """Tracked plus untracked-not-ignored *.rs files (git), else walk."""
    try:
        res = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others",
             "--exclude-standard", "--", "*.rs"],
            cwd=ROOT, capture_output=True, check=True,
        )
        names = [n.decode("utf-8", "surrogateescape")
                 for n in res.stdout.split(b"\0") if n]
        return sorted(n for n in set(names)
                      if os.path.isfile(os.path.join(ROOT, n)))
    except (OSError, subprocess.CalledProcessError):
        pass
    found = []
    for dirpath, dirs, files in os.walk(ROOT):
        if dirpath == ROOT:
            dirs[:] = [d for d in dirs if d not in SKIP_TOP]
        for name in files:
            if name.endswith(".rs"):
                rel = os.path.relpath(os.path.join(dirpath, name), ROOT)
                found.append(rel.replace(os.sep, "/"))
    return sorted(found)


def inventory():
    """Return ({file: (lines, over_100, over_79)}, undecodable files)."""
    out, bad = {}, []
    for rel in list_files():
        try:
            with open(os.path.join(ROOT, rel), "rb") as fh:
                raw = fh.read()
        except OSError as err:
            raise GuardError("cannot read %s: %s" % (rel, err.strerror))
        try:
            raw.decode("utf-8-sig")
        except UnicodeDecodeError:
            bad.append(rel)
        text = raw.decode("utf-8-sig", errors="replace")
        lines = text.split("\n")
        if lines and lines[-1] == "":
            lines.pop()
        widths = [width(line.rstrip("\r")) for line in lines]
        out[rel] = (
            len(lines),
            sum(w > HARD_WIDTH for w in widths),
            sum(w > SOFT_WIDTH for w in widths),
        )
    return out, bad


def measured(inv):
    """Current numbers in baseline shape (non-zero entries only)."""
    cur = {k: {} for k in KEYS}
    for f, (n, o100, o79) in inv.items():
        pkg = package_of(f)
        if n >= MAX_FILE_LINES:
            cur["oversize_files"][f] = n
        for key, val in (("lines_over_100", o100), ("lines_over_79", o79)):
            if val:
                cur[key][pkg] = cur[key].get(pkg, 0) + val
    return cur


def load_baseline():
    try:
        with open(BASELINE, encoding="utf-8") as fh:
            data = json.load(fh)
    except OSError as err:
        raise GuardError("cannot read baseline: %s" % err.strerror)
    except ValueError as err:
        raise GuardError("baseline is not valid JSON: %s" % err)
    if not isinstance(data, dict):
        raise GuardError("baseline must be a JSON object")
    if data.get("version") != VERSION:
        raise GuardError(
            "baseline version %r unsupported (need %d)"
            % (data.get("version"), VERSION)
        )
    for name, want in (("max_file_lines", MAX_FILE_LINES),
                       ("hard_width", HARD_WIDTH),
                       ("soft_width", SOFT_WIDTH)):
        if data.get(name) != want:
            raise GuardError(
                "baseline %s=%r does not match script (%d)"
                % (name, data.get(name), want)
            )
    out = {}
    for key in KEYS:
        table = data.get(key, {})
        if not isinstance(table, dict) or not all(
            isinstance(k, str) and isinstance(v, int)
            and not isinstance(v, bool) and v > 0
            for k, v in table.items()
        ):
            raise GuardError(
                "baseline %s must map names to positive integers" % key
            )
        out[key] = dict(table)
    return out


def render(data):
    doc = {
        "_comment": (
            "Existing size exceptions; every number may only shrink or be "
            "deleted. Regenerate downward with: python3 "
            "scripts/check_size.py --update. oversize_files is per file "
            "(files < 1000 lines). lines_over_100 (rustfmt max_width) and "
            "lines_over_79 are aggregate counts per package "
            "(crates/<name>, examples/<name>, benchmarks/<name>), so a "
            "file split inside a package keeps its budget. Packages not "
            "listed have a budget of 0."
        ),
        "version": VERSION,
        "max_file_lines": MAX_FILE_LINES,
        "hard_width": HARD_WIDTH,
        "soft_width": SOFT_WIDTH,
    }
    for key in KEYS:
        doc[key] = dict(sorted(data[key].items()))
    return json.dumps(doc, indent=2) + "\n"


def write_baseline(data):
    text = render(data)
    try:
        try:
            with open(BASELINE, encoding="utf-8") as fh:
                if fh.read() == text:
                    return False
        except OSError:
            pass
        with open(BASELINE, "w", encoding="utf-8") as fh:
            fh.write(text)
    except OSError as err:
        raise GuardError("cannot write baseline: %s" % err.strerror)
    return True


def compare(cur, base):
    """Return (violations, shrinkable) lists."""
    bad, stale = [], []
    for key in KEYS:
        for f, n in cur[key].items():
            allowed = base[key].get(f)
            if allowed is None:
                bad.append("%s: %d %s, not in baseline" % (f, n, MESSAGES[key]))
            elif n > allowed:
                bad.append("%s: %d %s, baseline allows %d"
                           % (f, n, MESSAGES[key], allowed))
            elif n < allowed:
                stale.append((key, f, n, allowed))
        for f, allowed in base[key].items():
            if f not in cur[key]:
                stale.append((key, f, 0, allowed))
    return bad, stale


def run(args):
    inv, undecodable = inventory()
    if undecodable:
        raise GuardError("not valid UTF-8: " + ", ".join(undecodable))
    cur = measured(inv)
    if args.init:
        if os.path.exists(BASELINE):
            raise GuardError("baseline exists; refusing to overwrite")
        write_baseline(cur)
        print("wrote %s" % os.path.relpath(BASELINE, ROOT))
        return 0
    base = load_baseline()
    bad, stale = compare(cur, base)
    if args.update:
        if bad:
            print("refusing to update; fix violations first:")
            for b in bad:
                print("  " + b)
            return 1
        new = {k: dict(base[k]) for k in KEYS}
        for key, f, n, _ in stale:
            if n:
                new[key][f] = n
            else:
                new[key].pop(f, None)
        changed = write_baseline(new)
        print("baseline shrunk: %d entries updated or removed" % len(stale)
              if changed else "baseline already up to date")
        return 0
    for key, f, n, allowed in stale:
        if n == 0:
            print("note: %s is now compliant for %s; remove entry (--update)"
                  % (f, key))
        else:
            print("note: %s %s shrank %d -> %d; ratchet with --update"
                  % (f, key, allowed, n))
    over = sorted(base["oversize_files"].items(), key=lambda kv: -kv[1])
    if over:
        print("deletion targets (files >= %d lines):" % MAX_FILE_LINES)
        for f, n in over:
            print("  %5d  %s" % (n, f))
    if bad:
        print("\nsize check FAILED:")
        for b in bad:
            print("  " + b)
        return 1
    if stale and args.strict:
        print("\nstale baseline entries; run --update and commit")
        return 1
    print("size check passed (%d files scanned)" % len(inv))
    return 0


def main(argv):
    parser = argparse.ArgumentParser(
        description="Rust size guard (see module docstring).")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--strict", action="store_true",
                      help="also fail on stale baseline entries")
    mode.add_argument("--update", action="store_true",
                      help="ratchet the baseline down only")
    mode.add_argument("--init", action="store_true",
                      help="create the baseline if missing")
    try:
        args = parser.parse_args(argv)
    except SystemExit as exc:
        return 0 if exc.code in (0, None) else 2
    try:
        return run(args)
    except GuardError as err:
        print("check_size: error: %s" % err, file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

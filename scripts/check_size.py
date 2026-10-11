#!/usr/bin/env python3
"""Size guard for Rust sources (roadmap R09).

Rules (see scripts/size_baseline.json and CONTRIBUTING.md):

* A file must stay below MAX_FILE_LINES (1,000) lines.
* No line may exceed HARD_WIDTH (100) characters, which is rustfmt's
  max_width; the guard therefore never fights `cargo fmt`.
* The number of lines above SOFT_WIDTH (79) in a file may not grow. Files
  not in the baseline have a budget of zero, so new code stays at 79
  columns (extract locals or shorten names; rustfmt keeps lines <= 100).

Existing violations are recorded in the baseline as exact numbers. They may
only shrink. Usage:

    python3 scripts/check_size.py            # check, exit 1 on violations
    python3 scripts/check_size.py --strict   # also fail on stale entries
    python3 scripts/check_size.py --update   # ratchet baseline down only
    python3 scripts/check_size.py --init     # create baseline if missing
"""

import json
import os
import sys

MAX_FILE_LINES = 1000  # files must be strictly below this
HARD_WIDTH = 100
SOFT_WIDTH = 79
SKIP_DIRS = {"target", ".git", "node_modules", "dist", ".astro"}
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASELINE = os.path.join(ROOT, "scripts", "size_baseline.json")
KEYS = ("oversize_files", "lines_over_100", "lines_over_79")


def inventory():
    """Return per-file (line_count, over_100, over_79) for all *.rs files."""
    out = {}
    for dirpath, dirs, files in os.walk(ROOT):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS)
        for name in sorted(files):
            if not name.endswith(".rs"):
                continue
            path = os.path.join(dirpath, name)
            rel = os.path.relpath(path, ROOT).replace(os.sep, "/")
            with open(path, encoding="utf-8") as fh:
                lines = fh.read().split("\n")
            if lines and lines[-1] == "":
                lines.pop()
            widths = [len(line.rstrip("\r")) for line in lines]
            out[rel] = (
                len(lines),
                sum(w > HARD_WIDTH for w in widths),
                sum(w > SOFT_WIDTH for w in widths),
            )
    return out


def measured(inv):
    """Violations currently present, in baseline shape."""
    return {
        "oversize_files": {
            f: v[0] for f, v in inv.items() if v[0] >= MAX_FILE_LINES
        },
        "lines_over_100": {f: v[1] for f, v in inv.items() if v[1]},
        "lines_over_79": {f: v[2] for f, v in inv.items() if v[2]},
    }


def load_baseline():
    with open(BASELINE, encoding="utf-8") as fh:
        data = json.load(fh)
    return {k: dict(data.get(k, {})) for k in KEYS}


def write_baseline(data):
    doc = {
        "_comment": (
            "Exact existing size exceptions; entries may only shrink or "
            "be deleted. Regenerate downward with: python3 "
            "scripts/check_size.py --update. Rules: files < 1000 lines; "
            "no line > 100 chars (rustfmt max_width); lines > 79 per "
            "file may not grow; unlisted files have a budget of 0. "
            "Oversize files are deletion targets: split them and "
            "remove the entry."
        ),
        "max_file_lines": MAX_FILE_LINES,
        "hard_width": HARD_WIDTH,
        "soft_width": SOFT_WIDTH,
    }
    for key in KEYS:
        doc[key] = dict(sorted(data[key].items()))
    with open(BASELINE, "w", encoding="utf-8") as fh:
        json.dump(doc, fh, indent=2)
        fh.write("\n")


MESSAGES = {
    "oversize_files": "lines (limit < %d)" % MAX_FILE_LINES,
    "lines_over_100": "lines wider than %d chars" % HARD_WIDTH,
    "lines_over_79": "lines wider than %d chars" % SOFT_WIDTH,
}


def compare(cur, base):
    """Return (violations, shrinkable) lists of strings."""
    bad, stale = [], []
    for key in KEYS:
        # Files absent from the baseline have a budget of zero.
        for f, n in cur[key].items():
            allowed = base[key].get(f)
            if allowed is None:
                bad.append("%s: %d %s, not in baseline" % (f, n, MESSAGES[key]))
            elif n > allowed:
                bad.append(
                    "%s: %d %s, baseline allows %d"
                    % (f, n, MESSAGES[key], allowed)
                )
            elif n < allowed:
                stale.append((key, f, n, allowed))
        for f, allowed in base[key].items():
            if f not in cur[key]:
                stale.append((key, f, 0, allowed))
    return bad, stale



def main(argv):
    mode = "check"
    for a in argv:
        if a in ("--update", "--init", "--strict"):
            mode = a[2:]
        elif a in ("-h", "--help"):
            print(__doc__)
            return 0
        else:
            print("unknown argument: %s" % a, file=sys.stderr)
            return 2
    inv = inventory()
    cur = measured(inv)
    if mode == "init":
        if os.path.exists(BASELINE):
            print("baseline exists; refusing to overwrite", file=sys.stderr)
            return 1
        write_baseline(cur)
        print("wrote %s" % os.path.relpath(BASELINE, ROOT))
        return 0
    base = load_baseline()
    bad, stale = compare(cur, base)
    if mode == "update":
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
        write_baseline(new)
        print("baseline shrunk: %d entries updated or removed" % len(stale))
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
    if stale and mode == "strict":
        print("\nstale baseline entries; run --update and commit")
        return 1
    print("size check passed (%d files scanned)" % len(inv))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

#!/usr/bin/env python3
"""Replay a review-policy/1 TOML file against a branch's recent landings.

A landing is one first-parent commit of the branch: a merge commit, or a
commit pushed straight to it. Each landing's diff is taken against its first
parent with rename detection, as the change request's Git facts are.

Usage:
  replay.py POLICY.toml [--branch origin/main] [--limit 50]
  replay.py --sizes [--branch origin/main] [--limit 200]

With a policy it prints:
  - each landing that a size or paths rule would have flagged, and why;
  - each landing that changes only evidence.ignore_paths files, which an
    earlier verify run would therefore cover (read every one of these);
  - each landing that version 0's ignore_paths would cover but this
    policy's does not, the files version 0 would have let through untested.
With --sizes it prints file and line percentiles and how many landings
version 0's limits and a few candidates would flag.

Needs Python 3.11 or later and git; run it from the repository root.
"""

import argparse
import subprocess
import sys

VERSION_0_IGNORE = ["docs/**", "**/*.md"]
VERSION_0_SIZE = {"max_changed_files": 40, "max_changed_lines": 1500}


def glob_match(glob, path):
    """The server's glob: whole path, ** is any number of whole segments,
    * stays in one segment, ? is one character other than /."""

    def segment(pattern, name):
        if not pattern:
            return not name
        if pattern[0] == "*":
            return any(segment(pattern[1:], name[i:]) for i in range(len(name) + 1))
        if name and (pattern[0] == "?" or pattern[0] == name[0]):
            return segment(pattern[1:], name[1:])
        return False

    def segments(globs, names):
        if not globs:
            return not names
        if globs[0] == "**":
            return any(segments(globs[1:], names[i:]) for i in range(len(names) + 1))
        return bool(names) and segment(globs[0], names[0]) and segments(globs[1:], names[1:])

    return segments(glob.split("/"), path.split("/"))


def matches(globs, path):
    return any(glob_match(g, path) for g in globs)


def landings(branch, limit):
    """[(sha, subject, [(path, old_path or None)], lines)] newest first."""
    out = subprocess.run(
        ["git", "log", branch, "--first-parent", "--diff-merges=first-parent", "-M",
         "--format=tformat:%x01%h %s", "--numstat", "-z", "-n", str(limit)],
        capture_output=True, text=True, check=True,
    ).stdout
    result = []
    for record in out.split("\x01")[1:]:
        header, _, rest = record.partition("\n")
        header = header.rstrip("\0")
        fields = rest.split("\0")
        files, lines, i = [], 0, 0
        while i < len(fields):
            entry = fields[i].strip("\n")
            i += 1
            if not entry:
                continue
            added, deleted, path = entry.split("\t", 2)
            old = None
            if path == "":  # a rename: the old and new paths follow
                old, path = fields[i], fields[i + 1]
                i += 2
            files.append((path, old))
            lines += 0 if added == "-" else int(added) + int(deleted)
        sha, _, subject = header.partition(" ")
        result.append((sha, subject, files, lines))
    return result


def percentile(values, p):
    values = sorted(values)
    return values[min(len(values) - 1, int(p * len(values)))] if values else 0


def sizes(rows):
    files = [len(f) for _, _, f, _ in rows]
    lines = [n for _, _, _, n in rows]
    print(f"{len(rows)} landings")
    for p in (0.5, 0.75, 0.9, 0.95):
        print(f"  p{int(p * 100)}: {percentile(files, p)} files, {percentile(lines, p)} lines")
    print(f"  max: {max(files, default=0)} files, {max(lines, default=0)} lines")
    for f_limit, l_limit in ((40, 1500), (20, 1000), (25, 2500), (50, 5000)):
        flagged = sum(f > f_limit or n > l_limit for f, n in zip(files, lines))
        print(f"  limits {f_limit} files / {l_limit} lines flag {flagged} of {len(rows)}")


def replay(policy, rows):
    ignore = policy.get("evidence", {}).get("ignore_paths", VERSION_0_IGNORE)
    size = {**VERSION_0_SIZE, **policy.get("size", {})}
    paths = policy.get("paths", [])
    covered, let_through, fired = [], [], {}
    print(f"== rules that fire, over {len(rows)} landings")
    for sha, subject, files, lines in rows:
        rules = []
        if len(files) > size["max_changed_files"]:
            rules.append(f"max_changed_files ({len(files)})")
        if lines > size["max_changed_lines"]:
            rules.append(f"max_changed_lines ({lines})")
        for entry in paths:
            if any(glob_match(entry["glob"], p) for p, _ in files):
                rules.append(f"paths {entry['glob']} ({entry['outcome']})")
        for rule in rules:
            fired[rule.split(" (")[0]] = fired.get(rule.split(" (")[0], 0) + 1
        if rules:
            print(f"  {sha} {subject[:60]}: {', '.join(rules)}")
        # A file is ignored when its path and, for a rename, old path match.
        def ignored(globs):
            return files and all(matches(globs, p) and (old is None or matches(globs, old)) for p, old in files)
        if ignored(ignore):
            covered.append((sha, subject, files))
        elif ignored(VERSION_0_IGNORE):
            let_through.append((sha, subject, [p for p, _ in files if not matches(ignore, p)]))
    print("  totals: " + (", ".join(f"{k} {v}" for k, v in fired.items()) or "none"))
    print("== landings an earlier verify run would cover (only ignore_paths files changed)")
    for sha, subject, files in covered:
        print(f"  {sha} {subject[:60]}: {', '.join(p for p, _ in files[:5])}{' ...' if len(files) > 5 else ''}")
    print("== landings version 0 would cover but this policy does not")
    for sha, subject, files in let_through:
        print(f"  {sha} {subject[:60]}: {', '.join(files[:5])}{' ...' if len(files) > 5 else ''}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("policy", nargs="?", help="the review-policy/1 TOML file")
    parser.add_argument("--sizes", action="store_true", help="print landing size percentiles instead")
    parser.add_argument("--branch", default="origin/main", help="branch to read (default origin/main)")
    parser.add_argument("--limit", type=int, help="landings to read (default 50, or 200 with --sizes)")
    args = parser.parse_args()
    if not args.sizes and not args.policy:
        parser.error("pass a policy file, or --sizes")
    try:
        rows = landings(args.branch, args.limit or (200 if args.sizes else 50))
    except subprocess.CalledProcessError as e:
        sys.exit(f"git log {args.branch} failed: {e.stderr.strip()}; fetch it or pass --branch")
    if args.sizes:
        sizes(rows)
        return
    import tomllib

    try:
        with open(args.policy, "rb") as f:
            policy = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError) as e:
        sys.exit(f"{args.policy}: {e}")
    replay(policy, rows)


if __name__ == "__main__":
    main()

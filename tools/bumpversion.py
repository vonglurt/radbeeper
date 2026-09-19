#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""Set the FIRST `version = "..."` line of a Cargo manifest, and only the first.

WHY THIS IS NOT ONE LINE OF SED. `sed 's|^version = ".*"|...|'` replaces every
match, and `gui/Cargo.toml` has two: the crate's own, and `[dependencies.iced]`
carrying `version = "0.14"`. Rewriting the second asked cargo for iced ^0.4.0 --
a real release, four years old, with none of the features the window uses -- so
the build failed on an unknown feature name and said nothing about the manifest
that had just been edited underneath it.

The GNU answer is `sed '0,/^version = /s|...|...|'`, which bounds the
substitution to the first match. This is Alpine: sed is busybox, busybox does
not implement that address, and it fails by changing NOTHING and exiting 0 --
which is worse than failing loudly, because the release then builds and tests a
version that was never bumped.

So: read the file, change the first matching line, write it back. It also
refuses to touch a file where that line is not where it should be, because the
whole failure above came of an edit landing somewhere nobody was looking.

    tools/bumpversion.py -v 0.4.0 Cargo.toml
    tools/bumpversion.py -v 0.4.0 --python radbeeper

THE REFERENCE PROGRAM CARRIES ITS OWN VERSION and has to be bumped with the
manifests. It is not a manifest -- it is `VERSION = "0.3.1"` in a Python file --
and forgetting it is not a cosmetic drift: both programs write the version into
the HTML they export, and that HTML is compared byte for byte, so the whole
differential suite fails at once pointing into the middle of index.html.
tests/test_version.py exists to say the cause instead, and it does; this exists
so nobody has to read it.
"""
import argparse
import re
import sys


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("-v", "--version", required=True, help="the version to set")
    p.add_argument("--python", action="store_true",
                   help="a Python file with VERSION = \"...\", not a manifest")
    p.add_argument("manifest")
    args = p.parse_args()

    if not re.match(r"^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$", args.version):
        sys.exit("bumpversion: %r is not a semver version" % args.version)

    with open(args.manifest, encoding="utf-8") as f:
        lines = f.read().split("\n")

    if args.python:
        for i, line in enumerate(lines):
            if re.match(r'^VERSION\s*=\s*"', line):
                lines[i] = re.sub(r'"[^"]*"', '"%s"' % args.version, line, count=1)
                with open(args.manifest, "w", encoding="utf-8") as f:
                    f.write("\n".join(lines))
                print("  %-22s VERSION = \"%s\"" % (args.manifest, args.version))
                return 0
        sys.exit("bumpversion: %s: no VERSION line found" % args.manifest)

    # THE FIRST ONE, AND IT HAS TO BE THE PACKAGE'S. A manifest whose first
    # `version =` sits under something other than [package] is not one this
    # understands, and guessing is how the iced line got rewritten.
    section = None
    for i, line in enumerate(lines):
        s = line.strip()
        if s.startswith("[") and s.endswith("]"):
            section = s
            continue
        if re.match(r'^version\s*=\s*"', line):
            if section != "[package]":
                sys.exit("bumpversion: %s: the first version line is under %s, "
                         "not [package]" % (args.manifest, section or "no section"))
            lines[i] = re.sub(r'"[^"]*"', '"%s"' % args.version, line, count=1)
            with open(args.manifest, "w", encoding="utf-8") as f:
                f.write("\n".join(lines))
            print("  %-22s version = \"%s\"" % (args.manifest, args.version))
            return 0
    sys.exit("bumpversion: %s: no version line found" % args.manifest)


if __name__ == "__main__":
    sys.exit(main())

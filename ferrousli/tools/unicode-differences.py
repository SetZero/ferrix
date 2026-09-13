#!/usr/bin/env python3
"""Explain where ferrousli's Unicode tables and the host glibc's C.UTF-8 differ.

    FERROUSLI_UNICODE_DIFFERENCES_OUT=/tmp/raw.txt cargo test --lib wctype
    python3 tools/unicode-differences.py /tmp/raw.txt > src/wctype/glibc-differences.txt

The unit test in src/wctype.rs compares every code point below U+110000 with
glibc and writes each run of consecutive code points that disagree the same
way. This script adds to each run the Unicode version that assigned its code
points, so that a difference of Unicode version can be told from a difference
of definition, and writes a header summing both up. The unit test ignores the
annotations and the header, and compares only the runs.

Ages come from Perl's Unicode::UCD. A code point Perl does not know but
Python's unicodedata does was assigned after Perl's Unicode version. A code
point neither knows, but which glibc gives a class, a case mapping or a width,
was assigned after Python's.
"""

from __future__ import annotations

import bisect
import collections
import re
import subprocess
import sys
import unicodedata

# The Unicode version of the tables, from musl 1.2.5's release notes.
MUSL_VERSION = (12, 1)

RUN = re.compile(
    r"^(\w+) U\+([0-9A-F]+)\.\.U\+([0-9A-F]+) (\d+) musl ([+-]\d+) glibc ([+-]\d+)$"
)

PERL = r"""
use Unicode::UCD;
my ($starts, $ages) = Unicode::UCD::prop_invmap("Age");
print "version\t", Unicode::UCD::UnicodeVersion(), "\n";
print "$starts->[$_]\t$ages->[$_]\n" for 0 .. $#$starts;
"""

EXPLANATIONS = """\
# Where musl's definitions differ from glibc's, whatever the version:
#
# * iswprint, iswgraph and wcwidth: musl treats every unassigned scalar
#   value, surrogate and noncharacter such as U+FDD0..U+FDEF as printable, of
#   width 1. glibc treats them as unprintable, of width -1.
# * wcwidth: symbols such as the hexagrams U+4DC0..U+4DFF and U+1D300..U+1D356
#   became wide after Unicode 12.1; musl's tables still give them width 1.
# * iswpunct: glibc's punct is every graphic character that is not
#   alphanumeric, so it includes the private-use characters U+E000..U+F8FF
#   and planes 15 and 16, the tags and the variation selectors. musl's table
#   is punctuation and symbols only, and holds the combining letters
#   U+0363..U+036F and U+1DD3..U+1DE6, which glibc calls alphabetic.
# * iswalpha and iswalnum: besides those, musl counts all of U+20000..U+2FFFD
#   as alphabetic, assigned or not.
# * iswlower and iswupper: musl means "has a mapping to the other case".
#   glibc means the Lowercase and Uppercase properties, which include letters
#   without one: U+00AA, U+0138, the phonetic letters from U+1D00, the
#   mathematical letters and the circled letters.
# * towlower and towupper: musl maps only letters, so not the circled letters
#   U+24B6..U+24E9, and it maps U+00DF to U+1E9E, which glibc does not.
# * iswspace and iswblank: musl excludes U+1680 and counts U+0085 as space;
#   its iswblank is only space and tab.
# * iswcntrl: musl includes U+FFF9..U+FFFB.
"""


def version_key(age: str) -> tuple[int, ...]:
    return tuple(int(part) for part in age.split("."))


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    raw = open(sys.argv[1], encoding="utf-8").read().splitlines()

    perl = subprocess.run(["perl", "-e", PERL], capture_output=True, text=True, check=True)
    lines = perl.stdout.splitlines()
    perl_version = lines[0].split("\t")[1]
    starts: list[int] = []
    ages: list[str] = []
    for line in lines[1:]:
        start, age = line.split("\t")
        starts.append(int(start))
        ages.append(age)
    python_version = unicodedata.unidata_version

    def age_of(cp: int, glibc_knows: bool) -> str:
        age = ages[bisect.bisect_right(starts, cp) - 1]
        if age != "Unassigned":
            return age
        if unicodedata.category(chr(cp)) != "Cn":
            return f"after {perl_version}"
        if glibc_knows:
            return f"after {python_version}"
        return "unassigned"

    def is_newer(age: str) -> bool:
        if age == "unassigned":
            return False
        if age.startswith("after "):
            return True
        return version_key(age) > MUSL_VERSION

    def order(age: str) -> tuple:
        if age == "unassigned":
            return (99,)
        if age.startswith("after "):
            return version_key(age[6:]) + (1,)
        return version_key(age)

    annotated: list[str] = []
    totals: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    for line in raw:
        match = RUN.match(line)
        if not match:
            print(f"unicode-differences: cannot read {line!r}", file=sys.stderr)
            return 1
        name, first, last, _, _, glibc = match.groups()
        glibc_value = int(glibc)
        if name == "wcwidth":
            glibc_knows = glibc_value != -1
        elif name in ("towlower", "towupper"):
            glibc_knows = glibc_value != 0
        else:
            glibc_knows = glibc_value != 0
        counts: collections.Counter = collections.Counter()
        for cp in range(int(first, 16), int(last, 16) + 1):
            age = age_of(cp, glibc_knows)
            counts[age] += 1
            kind = "newer" if is_newer(age) else ("unassigned" if age == "unassigned" else "older")
            totals[name][kind] += 1
        ages_text = ", ".join(f"{age}: {counts[age]}" for age in sorted(counts, key=order))
        annotated.append(f"{line}  # {ages_text}")

    out = sys.stdout
    out.write(
        "# Every code point below U+110000 where ferrousli's wide character\n"
        "# functions, on musl 1.2.5's Unicode 12.1.0 tables, disagree with the host\n"
        "# glibc's C.UTF-8. Written by the unit test in src/wctype.rs and\n"
        "# tools/unicode-differences.py; the test compares the runs and ignores\n"
        "# everything after `#`.\n"
        "#\n"
        "# A line is a function, a run of code points, its length, and what each\n"
        "# library returns: 0 or 1 for a class, the offset from the character for a\n"
        "# case mapping, the width for wcwidth. Its annotation counts the run's code\n"
        f"# points by the Unicode version that assigned them (Perl's UCD {perl_version},\n"
        f"# Python's {python_version}); \"after {python_version}\" means glibc knows a\n"
        "# character neither does, and \"unassigned\" means no one does.\n"
        "#\n"
        "# Code points assigned after Unicode 12.1 differ because glibc's data is\n"
        "# newer. Those assigned by 12.1 differ by definition, or because Unicode\n"
        "# changed their properties since.\n"
        "#\n"
        f"# {'function':<10} {'differ':>7} {'newer':>7} {'by 12.1':>8} {'unassigned':>11}\n"
    )
    for name in totals:
        t = totals[name]
        total = t["newer"] + t["older"] + t["unassigned"]
        out.write(
            f"# {name:<10} {total:>7} {t['newer']:>7} {t['older']:>8} {t['unassigned']:>11}\n"
        )
    out.write("#\n")
    out.write(EXPLANATIONS)
    out.write("\n")
    for line in annotated:
        out.write(line + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Extract the seccomp filters an `strace -f -v` of Chrome installed.

    extract.py chrome-v.strace data/

`strace -v` prints a filter as `{len=N, filter=[BPF_STMT(code, k), BPF_JUMP(code,
k, jt, jf), ...]}` in `seccomp(SECCOMP_SET_MODE_FILTER, flags, ...)`. Each
filter of more than a handful of instructions is written to
`data/chrome-<n>.bpf` as raw `struct sock_filter`s (code u16, jt u8, jf u8,
k u32, little-endian), in the order they were installed, skipping the probes
(`NULL`, which the kernel refuses) and any filter already written.

`data/chrome-<n>.verdicts` is then made on a Linux machine by `oracle.c`, which
installs the filter in a child and records what the real kernel does with each
call number. `chrome.rs` holds our interpreter to it.
"""

import re
import struct
import sys

SYMBOLS = {
    # Instruction classes, sizes, modes, operators and sources (bpf_common.h).
    "BPF_LD": 0x00, "BPF_LDX": 0x01, "BPF_ST": 0x02, "BPF_STX": 0x03,
    "BPF_ALU": 0x04, "BPF_JMP": 0x05, "BPF_RET": 0x06, "BPF_MISC": 0x07,
    "BPF_W": 0x00, "BPF_H": 0x08, "BPF_B": 0x10,
    "BPF_IMM": 0x00, "BPF_ABS": 0x20, "BPF_IND": 0x40, "BPF_MEM": 0x60,
    "BPF_LEN": 0x80, "BPF_MSH": 0xA0,
    "BPF_ADD": 0x00, "BPF_SUB": 0x10, "BPF_MUL": 0x20, "BPF_DIV": 0x30,
    "BPF_OR": 0x40, "BPF_AND": 0x50, "BPF_LSH": 0x60, "BPF_RSH": 0x70,
    "BPF_NEG": 0x80, "BPF_MOD": 0x90, "BPF_XOR": 0xA0,
    "BPF_JA": 0x00, "BPF_JEQ": 0x10, "BPF_JGT": 0x20, "BPF_JGE": 0x30,
    "BPF_JSET": 0x40,
    "BPF_K": 0x00, "BPF_X": 0x08, "BPF_A": 0x10,
    "BPF_TAX": 0x00, "BPF_TXA": 0x80,
    # seccomp.h
    "SECCOMP_RET_KILL_PROCESS": 0x80000000, "SECCOMP_RET_KILL_THREAD": 0,
    "SECCOMP_RET_KILL": 0,
    "SECCOMP_RET_TRAP": 0x00030000, "SECCOMP_RET_ERRNO": 0x00050000,
    "SECCOMP_RET_USER_NOTIF": 0x7FC00000, "SECCOMP_RET_TRACE": 0x7FF00000,
    "SECCOMP_RET_LOG": 0x7FFC0000, "SECCOMP_RET_ALLOW": 0x7FFF0000,
}


def value(expr):
    """An expression of symbols and numbers joined by `|`."""
    total = 0
    for part in expr.split("|"):
        part = part.strip()
        total |= SYMBOLS[part] if part in SYMBOLS else int(part, 0)
    return total


def split_args(text):
    """Top-level comma-separated arguments of a BPF_STMT/BPF_JUMP call."""
    return [piece.strip() for piece in text.split(",")]


def insns(body):
    out = []
    for match in re.finditer(r"BPF_(STMT|JUMP)\(([^()]*)\)", body):
        args = split_args(match.group(2))
        if match.group(1) == "STMT":
            code, k = value(args[0]), value(args[1])
            out.append((code, 0, 0, k))
        else:
            code, k, jt, jf = (value(a) for a in args)
            out.append((code, jt, jf, k))
    return out


def main():
    source, target = sys.argv[1], sys.argv[2]
    seen = set()
    written = 0
    with open(source, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if "SECCOMP_SET_MODE_FILTER" not in line or "{len=" not in line:
                continue
            body = line[line.index("{len="):]
            program = insns(body)
            declared = int(re.match(r"\{len=(\d+)", body).group(1))
            if len(program) != declared or declared < 8:
                continue
            blob = b"".join(struct.pack("<HBBI", *insn) for insn in program)
            if blob in seen:
                continue
            seen.add(blob)
            written += 1
            with open(f"{target}/chrome-{written}.bpf", "wb") as out:
                out.write(blob)
            print(f"chrome-{written}.bpf: {declared} instructions")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Print the export table of a PE image.

There is no dumpbin on macOS and llvm-readobj is not guaranteed to be on PATH, so
build.sh uses this to report what the cross-linked driver DLL actually exports. It is
informational only: a UMDF driver normally exports nothing.
"""

import struct
import sys


def rva_to_offset(sections, rva):
    for name, vsize, vaddr, rawsize, rawptr in sections:
        if vaddr <= rva < vaddr + max(vsize, rawsize):
            return rawptr + (rva - vaddr)
    return None


def main(path):
    with open(path, "rb") as handle:
        data = handle.read()

    if data[:2] != b"MZ":
        raise SystemExit(f"{path}: not a PE image (no MZ)")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise SystemExit(f"{path}: not a PE image (no PE signature)")

    machine, nsections = struct.unpack_from("<HH", data, pe + 4)
    opt_size = struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    if magic != 0x20B:
        raise SystemExit(f"{path}: not PE32+ (optional header magic 0x{magic:x})")

    ndirs = struct.unpack_from("<I", data, opt + 108)[0]
    dirs = opt + 112
    export_rva, export_size = struct.unpack_from("<II", data, dirs) if ndirs > 0 else (0, 0)

    sec = opt + opt_size
    sections = []
    for i in range(nsections):
        base = sec + i * 40
        name = data[base:base + 8].rstrip(b"\0").decode("ascii", "replace")
        vsize, vaddr, rawsize, rawptr = struct.unpack_from("<IIII", data, base + 8)
        sections.append((name, vsize, vaddr, rawsize, rawptr))

    print(f"machine=0x{machine:04x} sections={nsections} export_rva=0x{export_rva:x} size={export_size}")

    if export_rva == 0 or export_size == 0:
        print("exports: none")
        return

    off = rva_to_offset(sections, export_rva)
    if off is None:
        print("exports: export directory RVA not mapped by any section")
        return

    name_rva = struct.unpack_from("<I", data, off + 12)[0]
    ordinal_base = struct.unpack_from("<I", data, off + 16)[0]
    nfuncs, nnames = struct.unpack_from("<II", data, off + 20)
    names_rva, ordinals_rva = struct.unpack_from("<II", data, off + 32)

    dll_name = "?"
    name_off = rva_to_offset(sections, name_rva)
    if name_off is not None:
        end = data.index(b"\0", name_off)
        dll_name = data[name_off:end].decode("ascii", "replace")

    print(f"exports: dll_name={dll_name} functions={nfuncs} names={nnames} ordinal_base={ordinal_base}")

    names_off = rva_to_offset(sections, names_rva)
    ordinals_off = rva_to_offset(sections, ordinals_rva)
    if names_off is None or ordinals_off is None:
        return
    for i in range(nnames):
        entry_rva = struct.unpack_from("<I", data, names_off + i * 4)[0]
        entry_off = rva_to_offset(sections, entry_rva)
        ordinal = struct.unpack_from("<H", data, ordinals_off + i * 2)[0] + ordinal_base
        if entry_off is None:
            continue
        end = data.index(b"\0", entry_off)
        print(f"  {ordinal:5d}  {data[entry_off:end].decode('ascii', 'replace')}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: pe-exports.py <image.dll|image.exe>")
    main(sys.argv[1])

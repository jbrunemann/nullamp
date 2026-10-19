"""Write a Windows .res file holding the Nullamp application icon.
Standard library only, so cross-compiling from macOS or Linux needs no resource compiler (rc.exe, llvm-rc).
Usage: mkres.py OUT.res"""
import os, struct, sys
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from icon import render

SIZES = [16, 32, 48, 64, 128, 256]
RT_ICON, RT_GROUP_ICON = 3, 14

def resource(rtype, rid, data, flags=0x1030, lang=0x0409):
    # Fixed 32-byte header for numeric type and name, then the data padded to a DWORD.
    head = struct.pack("<IIHHHHIHHII", len(data), 32, 0xFFFF, rtype, 0xFFFF, rid, 0, flags, lang, 0, 0)
    return head + data + b"\0" * (-len(data) % 4)

def main(out):
    pngs = [(s, render(s, full=True)) for s in SIZES]
    # Vista and later read PNG payloads inside icons directly.
    group = struct.pack("<HHH", 0, 1, len(pngs))
    for i, (s, png) in enumerate(pngs, 1):
        group += struct.pack("<BBBBHHIH", s % 256, s % 256, 0, 0, 1, 32, len(png), i)
    blob = resource(0, 0, b"", 0, 0)  # the empty entry that marks a 32-bit .res
    for i, (_, png) in enumerate(pngs, 1):
        blob += resource(RT_ICON, i, png)
    blob += resource(RT_GROUP_ICON, 1, group)
    open(out, "wb").write(blob)

if __name__ == "__main__":
    main(sys.argv[1])

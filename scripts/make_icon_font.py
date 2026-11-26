#!/usr/bin/env python3
"""Rebuilds assets/fonts/lucide.ttf: the few Lucide icons the app uses.

Needs `pip install fonttools` and the Lucide icon font (npm `lucide-static`,
file `font/lucide.ttf` and `font/codepoints.json`). Usage:

    make_icon_font.py <lucide-static/font dir> <out.ttf>

Add an icon by adding its Lucide name to USED, then a `pub const` for it in
cmd/open-mtp-client/icons.rs (the code point is printed). The ligature tables
are dropped first: Lucide's trip an assertion in fontTools' subsetter, and the
app addresses icons by code point, never by name.
"""
import json
import os
import sys
import tempfile

from fontTools import subset
from fontTools.ttLib import TTFont

USED = """
folder file file-text file-image file-video file-audio file-archive package
chevron-left chevron-right chevron-up chevron-down arrow-up refresh-cw
layout-grid list x check minus equal house monitor hard-drive smartphone
download upload search folder-sync camera image music circle-x circle-check
triangle-alert info trash-2 message-circle file-code ellipsis image-down
brush-cleaning unplug
""".split()


def main(source, out):
    codepoints = json.load(open(os.path.join(source, "codepoints.json")))
    missing = [name for name in USED if name not in codepoints]
    if missing:
        sys.exit(f"not in Lucide: {missing}")
    font = TTFont(os.path.join(source, "lucide.ttf"), lazy=True)
    for table in ("GSUB", "GPOS", "GDEF"):
        if table in font:
            del font[table]
    with tempfile.TemporaryDirectory() as scratch:
        bare = os.path.join(scratch, "bare.ttf")
        font.save(bare)
        options = subset.Options()
        options.hinting = False
        options.layout_features = []
        options.name_IDs = ["*"]
        subsetter = subset.Subsetter(options)
        small = TTFont(bare)
        subsetter.populate(unicodes=[codepoints[name] for name in USED])
        subsetter.subset(small)
        small.save(out)
    for name in USED:
        print(f"{name:20} U+{codepoints[name]:04X}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])

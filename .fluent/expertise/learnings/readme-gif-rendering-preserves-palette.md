---
name: readme-gif-rendering-preserves-palette
description: Preserve FFmpeg's GIF palette and frame rectangles so README asset checks do not drift from redundant gifsicle optimization
metadata:
  type: gotcha
---

The README diagram renderer's `--check` mode compares generated assets byte for
byte. GIFs can display identical frames while differing in encoded bytes across
gifsicle versions, so visual equivalence alone does not satisfy that check.

`scripts/render-readme-diagrams.py` already quantizes animation frames through
FFmpeg's palette generation and palette use filters. Keep the subsequent
gifsicle pass at `-O0` to preserve that palette and the frame rectangles rather
than optimizing or quantizing them again.

When changing the rendering toolchain, retain the exact asset comparison and
inspect both themes, frame timing, and loop behavior before accepting regenerated
outputs. Consult `documentation/readme-diagrams/README.md` for the recorded
rendering environment; that provenance records a known result rather than
guaranteeing identical output from every future tool version.

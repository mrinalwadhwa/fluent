# How to consume YouTube content as an agent

Agents can't watch videos, but they can read transcripts.
YouTube auto-generates captions for most videos. Use yt-dlp to
download these captions as text.

## Setup

Install via nix: `nix-shell -p yt-dlp`

Or check if already available: `which yt-dlp`

## Downloading transcripts

Download auto-generated English subtitles without downloading the
video:

```bash
yt-dlp --write-auto-sub --sub-lang en --skip-download \
  --sub-format vtt -o "%(id)s" "https://youtube.com/watch?v=VIDEO_ID"
```

This produces a `.en.vtt` file (WebVTT format).

For multiple videos:

```bash
for vid in VIDEO_ID_1 VIDEO_ID_2 VIDEO_ID_3; do
  yt-dlp --write-auto-sub --sub-lang en --skip-download \
    --sub-format vtt -o "%(id)s" "https://youtube.com/watch?v=$vid"
done
```

## Cleaning VTT into readable text

VTT files contain timestamps and duplicate lines (auto-captions
repeat text across overlapping time segments). Clean them into
readable text:

```bash
# Strip timestamps and metadata, deduplicate lines
sed '/^$/d; /^[0-9]/d; /^WEBVTT/d; /^Kind:/d; /^Language:/d' \
  VIDEO_ID.en.vtt | awk '!seen[$0]++' > VIDEO_ID.txt
```

Or in Python for more control:

```python
import re

def clean_vtt(vtt_text):
    lines = []
    for line in vtt_text.splitlines():
        # Skip metadata, timestamps, and blank lines
        if not line.strip():
            continue
        if line.startswith(('WEBVTT', 'Kind:', 'Language:')):
            continue
        if re.match(r'^\d{2}:\d{2}', line):
            continue
        # Strip VTT formatting tags
        clean = re.sub(r'<[^>]+>', '', line).strip()
        if clean and (not lines or lines[-1] != clean):
            lines.append(clean)
    return ' '.join(lines)
```

## Working with transcripts

Auto-generated captions have no punctuation and imperfect word
boundaries. They're good enough for extracting key ideas but not
for direct quotation. When referencing a speaker's point:

- Paraphrase rather than quote directly
- Note that the source is an auto-generated transcript
- Use timestamps from the VTT if you need to reference a
  specific segment

## When transcripts aren't available

Some videos have captions disabled. yt-dlp will fail silently or
report no subtitles found. Check with:

```bash
yt-dlp --list-subs "https://youtube.com/watch?v=VIDEO_ID"
```

If no auto-generated subs exist, the video content is not
accessible to agents through this method.

## Batch research

When researching a topic across multiple videos (e.g., a
speaker's talks, a conference series), download all transcripts
first, then read them. This is more efficient than downloading
and reading one at a time.

```bash
mkdir -p transcripts
cd transcripts
for vid in ID1 ID2 ID3 ID4; do
  yt-dlp --write-auto-sub --sub-lang en --skip-download \
    --sub-format vtt -o "%(id)s" "https://youtube.com/watch?v=$vid"
done

# Clean all VTT files
for f in *.vtt; do
  sed '/^$/d; /^[0-9]/d; /^WEBVTT/d; /^Kind:/d; /^Language:/d' \
    "$f" | awk '!seen[$0]++' > "${f%.vtt}.txt"
done
```

Then read the .txt files to extract insights, compare positions,
and synthesize findings across sources.

# How to work with PDFs

## Contents

- Reading PDFs
  - PyMuPDF: the default
  - pdfplumber: for tables and reading order
  - Scanned PDFs and OCR
  - Metadata and page count
  - When rule-based tools fail
- Writing PDFs
  - Creating PDFs with Typst
  - Baseline grid
  - Page and text
  - Colors and accents
  - Headings
  - Layout techniques
  - Lists
  - Compiling and previewing
    - Direct PNG export (default)
    - Quick Look fallback for dense documents (macOS)
  - Iteration workflow
  - Slide decks (16:9)
  - Programmatic generation
  - When to use Typst

## Reading PDFs

Two libraries cover most tasks: `pymupdf` (imported as `fitz`) for text, metadata, OCR, and page count; `pdfplumber` for tables and multi-column reading order.

Install both in a venv:

```bash
python3 -m venv .venv && source .venv/bin/activate
pip install pymupdf pdfplumber
```

### PyMuPDF: the default

Use pymupdf for text extraction, metadata, page count, images, and OCR.

```python
import fitz  # pymupdf

doc = fitz.open("document.pdf")
for page in doc:
    print(page.get_text())
```

pymupdf is AGPL-3.0. If that's a problem (embedding in closed-source products), swap to `pypdfium2` — similar API, BSD-licensed.

### pdfplumber: for tables and reading order

pymupdf's table detection is rudimentary — it finds cells but doesn't reliably group them into rows and columns. Use pdfplumber when tables matter.

```python
import pdfplumber

with pdfplumber.open("statement.pdf") as pdf:
    for page in pdf.pages:
        for table in page.extract_tables():
            for row in table:
                print(row)
```

Also use pdfplumber for multi-column academic or legal PDFs. pymupdf can return text in PDF stream order, which doesn't match left-to-right, top-to-bottom reading. If a two-column paper reads as interleaved gibberish through pymupdf, switch to pdfplumber.

### Scanned PDFs and OCR

A scanned PDF is an image of paper — `page.get_text()` returns empty strings. Detect that case, then use pymupdf's built-in OCR:

```python
import fitz

doc = fitz.open("scanned.pdf")
for page in doc:
    text = page.get_text()
    if not text.strip():
        tp = page.get_textpage_ocr(language="eng", dpi=300)
        text = page.get_text(textpage=tp)
    print(text)
```

Requires the `tesseract` binary installed (`brew install tesseract` on macOS). For non-Latin scripts, install the matching language pack (`brew install tesseract-lang` on macOS) and pass `language="hin"` for Hindi, `"tam"` for Tamil, `"chi_sim"` for simplified Chinese. Raise DPI to 400–600 for dense scripts or low-resolution scans.

### Metadata and page count

```python
import fitz

doc = fitz.open("document.pdf")
print(len(doc))          # page count
print(doc.metadata)      # {'producer': 'Typst', 'creationDate': ...}
```

Born-digital PDFs have a real producer ("Typst", "LaTeX", "Microsoft Word", "Pages"); scanned ones show a scanner name or leave producer unset — a signal that OCR is needed.

### When rule-based tools fail

Both pymupdf and pdfplumber struggle with scientific PDFs containing heavy math, chemical structures, or complex figures. If plain text comes out garbled from both, reach for a transformer-based extractor: Nougat (specialized for scientific papers), Marker, or Docling. These need PyTorch and are much slower per page but are the only reliable option for this document class.

## Writing PDFs

### Creating PDFs with Typst

Typst is a modern typesetting system that compiles markup to PDF. It replaces LaTeX for most document types: reports, invoices, letters, presentations, technical documents. Typst compiles in milliseconds, has readable syntax, and produces clean PDF output.

Install via nix: `nix-shell -p typst`

### Baseline grid

Derive all vertical spacing from a single baseline unit. This creates consistent rhythm throughout the document. Every margin, paragraph gap, heading space, and list spacing should be a multiple of the baseline.

```typst
#let baseline = 14pt

#let sp-half    = baseline * 0.5   // tight: related elements
#let sp-one     = baseline         // standard: paragraphs
#let sp-onehalf = baseline * 1.5   // medium: before sub-headings
#let sp-two     = baseline * 2     // large: before sections
```

Set page margins, paragraph leading, and list spacing as multiples of the baseline. When the entire document uses one number as its source of rhythm, changing it retunes everything.

```typst
#set page(margin: (
  top: baseline * 2,
  bottom: baseline,
  left: baseline * 2.5,
  right: baseline * 2.5,
))
#set par(leading: baseline * 0.4, spacing: sp-one)
```

### Page and text

Choose a readable font at 9-10pt for dense documents, 10-12pt for longer-form documents. Set `justify: false` for short documents; justify for reports and articles.

```typst
#set text(font: "Meta Serif OT", size: 9.5pt, fill: rgb("#000000"))
```

### Colors and accents

Define a small palette. One accent color is enough for most documents. Use it for section headings, links, and decorative elements. Keep body text black.

```typst
#let accent = rgb("#BE4728")
#let muted = rgb("#606060")
#show link: it => text(fill: accent)[#it]
```

### Headings

Style each heading level explicitly. Headings use `#show` rules to control size, weight, color, and spacing. Add vertical space before headings using the baseline grid multiples.

```typst
#show heading.where(level: 1): it => {
  text(size: 22pt, weight: "bold")[#it.body]
  v(sp-half)
}

#show heading.where(level: 2): it => {
  v(sp-half)
  text(size: 10.5pt, weight: "bold", fill: accent)[#it.body]
  v(sp-half * 0.5)
}
```

### Layout techniques

Use `#place` for elements outside the normal flow: decorative bars, contact info in corners, watermarks.

```typst
// Red bar at page top
#place(top + left, dx: -baseline * 2.5, dy: -baseline * 2,
  rect(width: 100% + baseline * 5, height: 2pt, fill: accent)
)

// Contact info top-right
#place(top + right, dy: -sp-half, text(size: 8.5pt, fill: muted)[
  San Francisco | email@example.com
])
```

Use `#h(1fr)` to push content to the right on the same line:

```typst
=== Job Title #h(1fr) #text(size: 8.5pt, fill: muted)[2020–Present]
```

Use pipe separators (`|`) with `#h(0.3em)` spacing for inline metadata: `Company | Role | Dates`.

### Lists

Configure list markers, indent, and spacing to match the baseline grid:

```typst
#set list(
  marker: text(fill: rgb("#000000"))[•],
  indent: baseline * 1.2,
  body-indent: sp-half,
  spacing: sp-one,
)
```

### Compiling and previewing

Compile to PDF:

```bash
typst compile document.typ document.pdf
```

#### Direct PNG export (default)

Typst can render any page directly to PNG in a single command. This is the right default for iteration — no extra tools, portable across macOS and Linux, no temp files. Use `--pages N` to render one page, `--ppi` to choose resolution (144 is usually enough), and `{p}` in the output template for the page number.

```bash
typst compile --format png --ppi 144 --pages 3 document.typ "page-{p}.png"
```

For a multi-page range, drop `--pages` and Typst will emit one PNG per page using the `{p}` template.

#### Quick Look fallback for dense documents (macOS)

For documents with very small body text (resumes, dense reports at 8–10pt) the direct PNG can read slightly soft compared to macOS's native PDF renderer. If you can't read the rendered output crisply, fall back to the Quick Look chain — `gs` splits a single page out, `qlmanage` renders it via the system PDF renderer:

```bash
gs -dNOPAUSE -dBATCH -dQUIET -sDEVICE=pdfwrite \
  -dFirstPage=1 -dLastPage=1 -sOutputFile=page1.pdf document.pdf
qlmanage -t -s 2000 -o . page1.pdf
```

This is macOS-only and adds a `gs` dependency, so reach for it only when the direct PNG isn't crisp enough for what you need to verify. For slide decks, dashboards, and anything with body text ≥ 12pt, the direct export is fine.

### Iteration workflow

Visual verification is required for every change. Don't skip it — the rendered output catches typography, spacing, and overflow issues the source can't.

1. Edit the .typ source.
2. Compile to PDF: `typst compile document.typ document.pdf`.
3. Render the changed page to PNG (see "Direct PNG export" above).
4. **Read the PNG.** Use the image-read tool — don't just check the file exists.
5. Assess against the requirements. If something's off, adjust and repeat.

Typst compiles fast enough to iterate in tight loops. Don't batch changes: make one adjustment, compile, read, assess. The model is much better at catching layout problems by looking at the rendered page than by reasoning about the source.

### Slide decks (16:9)

For presentations, switch the page to a Keynote-shaped canvas (13.333 × 7.5 in) and use `#pagebreak()` between slides. Type sizes run larger than print: headlines around 44pt display bold, body around 22pt serif. Use the same baseline grid pattern as print docs — `bl = 22pt` is a common choice for 16:9.

```typst
#set page(width: 13.333in, height: 7.5in, margin: 22pt)
#let bl = 22pt
```

Render any slide for verification by passing its page number with `--pages`. Section each slide with a comment header so the source stays navigable:

```typst
// ============================================================
// SLIDE — <short name>
// ============================================================
```

### Programmatic generation

When a document has many repetitive structures — slides with shared layouts, line items in a report, recurring callouts — drive Typst from a Python (or other) script that emits the `.typ` source. Keep the content in real data structures (lists, dicts) and assemble the Typst as an f-string template. Editing one constant retunes every instance, and the data and presentation stay separate.

The convention: `generate_*.py` is the source of truth; the generated `.typ` is regenerated on every change and never hand-edited.

### When to use Typst

Typst works well for structured documents with predictable layouts: reports, invoices, letters, academic papers, technical documents, and slide decks. For documents that need complex interactive elements, forms, or dynamic content, other tools may be more appropriate.

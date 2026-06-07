# How to work with PDFs

## Creating PDFs with Typst

Typst is a modern typesetting system that compiles markup to PDF.
It replaces LaTeX for most document types: reports, invoices,
letters, presentations, technical documents. Typst compiles in
milliseconds, has readable syntax, and produces clean PDF output.

Install via nix: `nix-shell -p typst`

## Baseline grid

Derive all vertical spacing from a single baseline unit. This
creates consistent rhythm throughout the document. Every margin,
paragraph gap, heading space, and list spacing should be a
multiple of the baseline.

```typst
#let baseline = 14pt

#let sp-half    = baseline * 0.5   // tight: related elements
#let sp-one     = baseline         // standard: paragraphs
#let sp-onehalf = baseline * 1.5   // medium: before sub-headings
#let sp-two     = baseline * 2     // large: before sections
```

Set page margins, paragraph leading, and list spacing as
multiples of the baseline. When the entire document uses one
number as its source of rhythm, changing it retunes everything.

```typst
#set page(margin: (
  top: baseline * 2,
  bottom: baseline,
  left: baseline * 2.5,
  right: baseline * 2.5,
))
#set par(leading: baseline * 0.4, spacing: sp-one)
```

## Page and text

Choose a readable font at 9-10pt for dense documents, 10-12pt
for longer-form documents. Set `justify: false` for short
documents; justify for reports and articles.

```typst
#set text(font: "Meta Serif OT", size: 9.5pt, fill: rgb("#000000"))
```

## Colors and accents

Define a small palette. One accent color is enough for most
documents. Use it for section headings, links, and decorative
elements. Keep body text black.

```typst
#let accent = rgb("#BE4728")
#let muted = rgb("#606060")
#show link: it => text(fill: accent)[#it]
```

## Headings

Style each heading level explicitly. Headings use `#show` rules
to control size, weight, color, and spacing. Add vertical space
before headings using the baseline grid multiples.

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

## Layout techniques

Use `#place` for elements outside the normal flow: decorative
bars, contact info in corners, watermarks.

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

Use pipe separators (`|`) with `#h(0.3em)` spacing for inline
metadata: `Company | Role | Dates`.

## Lists

Configure list markers, indent, and spacing to match the baseline
grid:

```typst
#set list(
  marker: text(fill: rgb("#000000"))[•],
  indent: baseline * 1.2,
  body-indent: sp-half,
  spacing: sp-one,
)
```

## Compiling and previewing

Compile to PDF:
```bash
typst compile document.typ document.pdf
```

Split pages for preview using ghostscript:
```bash
gs -dNOPAUSE -dBATCH -dQUIET -sDEVICE=pdfwrite \
  -dFirstPage=1 -dLastPage=1 -sOutputFile=page1.pdf document.pdf
```

Preview pages as images using Quick Look (macOS):
```bash
qlmanage -t -s 2000 -o . page1.pdf
```

## Iteration workflow

1. Edit the .typ source
2. Compile: `typst compile document.typ document.pdf`
3. Preview: open the PDF or render page images
4. Read the output and assess against the user's requirements
5. Adjust spacing, font, or layout as needed
6. Repeat until the document looks right

Typst compiles fast enough to iterate in tight loops. Don't
batch changes: make one adjustment, compile, assess.

## When to use Typst

Typst works well for structured documents with predictable
layouts: reports, invoices, letters, academic papers, technical
documents. For documents that need complex interactive elements,
forms, or dynamic content, other tools may be more appropriate.

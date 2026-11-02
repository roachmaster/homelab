# Rebuild the remote IDE visual proposal

The [six-page PDF](remote-ide-proposal.pdf) is a visual companion to the
[architecture proposal](remote-ide.md). It does not claim that a remote IDE,
node agent or deployment is implemented.

## Inputs

- Current-code baseline: `a11d327a3fd1aadec232a47f1c7cc8d086a7ef0f`.
- Canonical diagrams: `m13-remote-ide-context` through `m17-remote-ide-bootstrap`
  under [`../diagrams/mermaid`](../diagrams/mermaid/README.md#rebuild-locally).
- Narrative, layout and links: [`build_remote_ide_pdf.py`](build_remote_ide_pdf.py).
- Pages: context, components, contracts, request, bootstrap and phased acceptance.

Rebuild the canonical SVGs with the gallery's documented pinned tools before
rebuilding the PDF when Mermaid sources change. The PDF builder does not render
Mermaid or install dependencies. It converts the checked SVGs to vector PDFs with
Inkscape and places those vectors into a ReportLab layout using PyMuPDF. An SVG 1.1 compatibility pass translates CSS rgba paint colors to equivalent hex
and opacity in temporary copies only. Text stays
selectable; diagrams are not screen captures. PDF outlines, internal page links,
source links and links to full-size SVGs are included.

## Build

Requirements: Python 3.9+, `reportlab`, `PyMuPDF`, and `inkscape` on `PATH`.
DejaVu Sans fonts are recommended to match the diagrams and embedded body text.
The builder uses standard Helvetica for body text if DejaVu Sans is unavailable;
that fallback can alter line wrapping and needs fresh visual review.

From the repository root:

```sh
python3 docs/architecture/proposals/build_remote_ide_pdf.py
```

To inspect intermediate vector files without changing the tracked PDF:

```sh
python3 docs/architecture/proposals/build_remote_ide_pdf.py \
  --output /tmp/remote-ide-proposal.pdf \
  --keep-intermediates /tmp/remote-ide-pdf-build
```

This creates only the selected PDF and optional intermediate directory. It makes
no network requests and does not run or deploy Forge. Output byte identity can vary
with font, Inkscape and Python package versions; verify content and layout, not just
a file hash across different authoring environments.

## Review before publication

Render every page, for example with Poppler:

```sh
pdftoppm -r 144 -png /tmp/remote-ide-proposal.pdf /tmp/remote-ide-page
pdfinfo /tmp/remote-ide-proposal.pdf
```

Inspect all six pages for readable labels, clipped text, overlaps and correct
current-versus-proposed status. Confirm six outline entries, working internal
navigation and expected GitHub links. Check the canonical Mermaid syntax and SVG
freshness with the gallery's documented gates. The linked proposal and new SVG
paths must exist in the published branch before announcing those live links.
When code or scope changes, update the baseline, diagrams and explanatory text
together; do not relabel a proposal as implemented without new source evidence.

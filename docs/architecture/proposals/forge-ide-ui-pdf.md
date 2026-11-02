# Rebuild the Forge IDE UI architecture report

The [eight-page report](forge-ide-ui.pdf) accompanies the
[UI architecture proposal](forge-ide-ui.md). The older
[remote IDE proposal PDF](remote-ide-proposal.pdf) is intentionally unchanged.
This report describes new UI/host work; it does not implement a browser app,
provision credentials or deploy a service.

> **Historical snapshot notice — 2 November 2026, 14:56 UTC:** the PDF and its
> generator preserve the earlier proposal. The later authoring constraint in the
> [current UI skill](../../../.agents/skills/design-forge-ui/SKILL.md) and
> [updated proposal](forge-ide-ui.md#5-build-output-is-not-runtime-behavior)
> supersedes its authored-template/worker-JS guidance: application web assets are
> generated from YAML/Rust component declarations. Rebuilding this unchanged
> generator reproduces the older narrative; it does not update that guidance.
> A future PDF revision must update its generator and diagrams together.

## Inputs and boundaries

- Current-code baseline: `175315b4fe1c993f15bb2ff52d824ee4cdaecc88`.
- Canonical Mermaid SVGs: M19 through M24 under
  [`../diagrams/mermaid`](../diagrams/mermaid/README.md#rebuild-locally).
- Narrative and layout: [`build_forge_ide_ui_pdf.py`](build_forge_ide_ui_pdf.py).
- Public browser references are linked on the request, ownership and security pages.
- Reference-project findings are summarized as design lessons; no private source,
  pins, credentials or operational configuration are embedded.

The builder embeds the canonical diagrams as PDF vectors using Inkscape and
PyMuPDF, on a ReportLab layout. A temporary SVG 1.1 compatibility pass converts
CSS rgba paint to equivalent hex and opacity; canonical SVGs are not modified.
Text stays selectable. The PDF includes eight outline entries, internal navigation,
pinned current-source links and live proposal/gallery/full-size SVG links.

## Build

Requirements: Python 3.9+, `reportlab`, `PyMuPDF` and `inkscape` on `PATH`.
DejaVu Sans is recommended for consistent fonts; a Helvetica fallback changes
body wrapping and requires fresh visual review. Rebuild and validate the canonical
SVGs with the gallery's documented tools before rebuilding this PDF.

From the repository root:

```sh
python3 docs/architecture/proposals/build_forge_ide_ui_pdf.py
```

For a separate inspection build:

```sh
python3 docs/architecture/proposals/build_forge_ide_ui_pdf.py \
  --output /tmp/forge-ide-ui.pdf \
  --keep-intermediates /tmp/forge-ide-ui-pdf-build
pdftoppm -r 144 -png /tmp/forge-ide-ui.pdf /tmp/forge-ide-ui-page
pdfinfo /tmp/forge-ide-ui.pdf
```

The builder makes no network requests and executes no Forge runtime. It writes
only the selected PDF and optional intermediates. Byte identity can vary across
fonts/exporter versions even when the report renders identically.

## Review before publication

Inspect all eight rendered pages for readable diagram labels, clipping, overlaps,
status wording and ownership/security consistency. Check that current agent behavior
is not mislabeled proposed, and that UI, host identity/session bootstrap, browser
contracts and tests are not claimed implemented. Validate eight bookmarks and all
internal destinations; verify every external URL against its intended source.
New main-branch links must be live before announcing the report. Check Mermaid
syntax, SVG freshness and relative documentation links with the gallery gates.

Acceptance tests in the report are future implementation requirements. Document
checks do not prove browser functionality, a secure session system or deployment.
Keep the PDF, canonical diagrams and proposal in sync when the design changes.

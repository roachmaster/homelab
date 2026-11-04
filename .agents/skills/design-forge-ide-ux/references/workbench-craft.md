# Workbench craft

Forge-specific adaptation, 2026-11-03, of design-taste app-screen guidance,
Impeccable's operational-interface principles and the VoltAgent reference index.
See [sources, changes and licenses](../../../../docs/design/skill-sources.md).
This is a design aid, not an instruction to run their tools or copy a product skin.

## Start from the work

Name the user's immediate task in the admitted project: find/select a file, work
with its supported content, or open a contextual analysis tool. Make that work and
the next supported action easy to find. Forge is an IDE/workbench, not a report
viewer with an IDE skin. Today's metadata-only capability does not make reports,
statistics or explanations the permanent center of the product. Preserve a successful
existing arrangement during polish; a request to improve spacing does not authorize
a new navigation system, rewritten product claims or a wholesale redesign.

Inventory real DTO fields, actions, bounded data and unavailable information before
choosing a look. A fact repeated in chrome, details or counts comes from the same
accepted Rust view, not separately authored sample numbers or JS calculations.
Keep zero, no matches, not inspected, denied and unavailable distinct. A partial
report needs a concise visible status; detailed limits belong in a discoverable
supporting disclosure, without drowning the selected work or hiding a consequence.

## Use references deliberately

The VoltAgent collection is an index of design analyses, not an IDE specification.
If a reference helps, record its exact document/revision or retrieval date, what
observable property it informs, and why it fits the workbench task. Some entries
now redirect to an external site: a pinned Git commit does not pin that site's
content. Do not claim an external design system was reviewed when only an index
or redirect was available.

Extract a useful relationship, such as quiet tool chrome around a dominant work
area or consistent typographic hierarchy. Do not transplant a marketing hero,
brand assets, arbitrary color percentages, typeface, icon library or card grid.
Use the project's existing tokens and original glyphs first. Any reference-led
change still maps to YAML tokens/typed Rust components and the real generator.

## Precision over decoration

- Give the selected file/work area space. Keep recognizable Explorer navigation
  and compact secondary tools, with aligned rows and stable pane geometry.
  Disclose metadata, inventory and metrics progressively instead of placing them
  beside the work as equal-weight panels. Borders and subtle surface differences can group
  dense tools; removing all boundaries can make the workbench harder to read.
- Use a small, legible type hierarchy. Distinguish prose, paths and numeric spans
  without making every label tiny, uppercase or monospace. Preserve complete names
  through wrapping, details or another keyboard-accessible representation.
- Use emphasis for selected/focused content, primary action and meaningful status.
  Keep focus visibly distinct from selection. Pair status color with words or
  symbols, and check legibility in every implemented theme and disabled state.
- Keep symbols, labels, row alignment, hit areas and spacing coherent. A familiar
  tree or table can be right for an IDE; originality is not a reason to discard a
  usable convention. Avoid unrelated metric tiles, gradients and ambient effects.
- Errors should say what failed, what remains valid and the supported recovery.
  Do not replace a precise admission failure with a vague success-looking empty
  screen or promise a retry that does not exist.

These are review criteria, not numerical proof of quality. The upstream app-screen
study is desktop-focused and excludes dense operations tables and loading/error
screens. Its observations do not validate Forge's keyboard, narrow or adverse flows.

Before polishing, apply the UX skill's [workbench guardrails](../SKILL.md#workbench-guardrails)
and the QA skill's [acceptance checklist](../../test-forge-ui/SKILL.md#workbench-acceptance-checklist).
A beautiful inspection dashboard is still the wrong primary design. Contextual
inspection tools are valid; the issue is their hierarchy, not their existence.

## Inspect, correct, verify

For an authorized UI change, inspect fresh desktop and narrow rendered views with
long names, empty/partial/denied data and keyboard focus, not only a polished happy
path. Compare actual screenshots with declared intent. Check clipping, scroll
ownership, contrast, row density and hierarchy; use accessibility structure and
real interactions to verify names and behavior. Neither pixels alone nor a tree
snapshot establishes full accessibility conformance.

Batch the related corrections from a review, regenerate assets, then look again.
Fix the authored tokens/components or a scoped reusable generator primitive, never
patch generated HTML/CSS/JS. For motion, use the UI skill's
[motion reference](../../design-forge-ui/references/motion.md). Run the existing
browser gate when implementation changes require it and label unrun checks.

Report specific findings with the affected view/state, evidence, user impact,
authored source to change and verification needed. Separate functional defects,
legibility/accessibility problems and optional taste choices; do not present a
house preference or a static mockup as a proven usability result.

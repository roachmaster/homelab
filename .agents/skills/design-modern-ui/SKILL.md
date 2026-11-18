---
name: design-modern-ui
description: Define and verify visual hierarchy, differentiated launchers and documentation, responsive spacing, and accessible dark surfaces before creating or updating Forge pages and frontend components. Pair with Forge's declarative UI and browser verification skills.
---

# Design Modern UI

Apply these quality rules before changing the UI declarations. Start with
[Wire Forge Providers](../wire-forge-providers/SKILL.md) for capability wiring
and [Design Forge UI](../design-forge-ui/SKILL.md) for generated asset and
execution boundaries. Keep the change scoped to the requested screen.

## Set the hierarchy before authoring

Identify the screen's main task, primary action, supporting links, and metadata.
Assign distinct visual treatments before choosing a component layout.

- Give the primary action a solid, high-contrast fill and a clear action label.
  Keep secondary actions quieter. Group related hero actions in a wrapping row
  with a 12px gap, fitting both at standard phone widths where labels allow it.
- Present app launchers as clickable cards with a recognizable 16–20px line
  icon, title, short task description, and trailing arrow. Make the entire
  card a single native link. Use available typed icon primitives and consistent
  stroke weights. Decorative icons and arrows are hidden from assistive text.
- Present documentation as compact rows or a responsive grid with restrained
  dividers. Keep reading links visually lighter than launch actions.
- Put repository and project metadata in a minimal footer. Fold related
  screen descriptions into the appropriate launcher or documentation entry.
- Give repeated destinations distinct roles only when that helps navigation.
  Avoid repeating the same primary launch in the header, hero, and first card.

A compact context eyebrow can identify the workspace above the title. Use a
status badge only when its words reflect available, current data. Static pages
can name the workspace without claiming runtime readiness. Include tags only
when they distinguish useful categories. Keep filler counters, decorative
numbering, and redundant explanatory text out of the page.

## Use consistent surfaces and typography

Reuse the screen's theme and token system. For the Forge portal dark theme,
use a base canvas near `#0b0e14`, an elevated surface near `#121721`, and an
interactive surface near `#181f2c`. Keep palette changes local to the screen
unless theme changes are requested. Use crisp 1px borders with a subtle
translucent neutral, such as `rgba(255, 255, 255, 0.08)`. Reserve brighter
borders and accents for interaction or emphasis.

Use one clear title, concise section headings, and quieter descriptions. A
portal title can use `clamp(1.75rem, 3vw, 2.5rem)` with tight tracking. Small
section labels can use 0.75rem text with uppercase and 0.08em tracking.
Descriptions can use 0.9375rem text at 1.5 line height. Preserve readable
contrast and complete labels at every supported width.

Give clickable cards and buttons explicit hover and focus-visible styles.
Use a modest border, background, or elevation change with a 150ms transition.
Keep keyboard focus clearly visible. Suppress nonessential transitions and
movement for `prefers-reduced-motion`. Keep interactive hit areas at least
44px high and wide where the control shape permits.

## Control the layout rhythm

Use a centered responsive container around 960–1120px for portal launchers.
Use 8–12px spacing within related groups and 32–48px between major sections.
At wider viewports, let repeated entries form a responsive grid. On narrow
screens, stack cards while preserving the distinction between actions and
documentation. Avoid nested panels around already bounded cards. Keep the
selected file or task area dominant in the IDE and use its existing layout
contracts for dense tools.

Author structure through typed Rust components and styles through YAML tokens
or Rust property declarations. Use the existing generator and reusable renderer.
Keep the browser, worker, and Rust responsibilities described in Design Forge
UI intact while changing appearance.

## Check the rendered result

Use [Test Forge UI](../test-forge-ui/SKILL.md) for the relevant browser path.
Inspect the screen while its changed content is visible at phone and desktop
sizes. Check that launchers and documentation read differently, the primary
action has a solid fill, and metadata stays subordinate. Verify that labels,
icons, focus rings, and hover surfaces remain legible without clipping.

Exercise keyboard entry and the changed links, measure the mobile hit areas,
and check that major groups stack or form columns as intended. Verify actual
computed contrast where it is consequential to the change. Test any added
motion with reduced motion enabled. Report the viewport sizes, checks run,
and any remaining visual or behavior issue.

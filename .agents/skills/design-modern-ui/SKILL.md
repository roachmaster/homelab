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

## Mobile chrome budget

On mobile viewports narrower than 640px, never stack more than two header or
navigation bars above the primary content. Combine identity and the active
context in one compact header, followed by one compact navigation strip. Move
secondary actions such as Manage, History, and Settings into a drawer, overflow
menu, or title dropdown. Keep touch targets and meaningful accessible names
when using icons. Put contextual editing controls with their content rather
than adding another global navigation tier.

Avoid wrapping scrollable feeds inside bordered containers. Give the main
conversation or feed the remaining viewport height and disclose management
controls progressively so the user can read before managing the workspace.
Verify this budget in the rendered phone view with the ordinary drawer closed;
loading and actionable errors must still remain available.

## Email view pattern

Present Mail as correspondence with an inbox, a thread reader, and an explicit
compose state. Use the existing Rust model and generated component contracts.

- Use full-width inbox rows separated by hairline dividers. Show sender or
  model identity and a compact date first, the subject next, then a bounded
  one- or two-line body preview. Keep the entire row one keyboard-accessible
  action. Wrap long subjects and clamp previews without causing page overflow.
- Give Inbox and Archive a quiet underline treatment and an adjacent refresh
  icon. Put Compose in a clearly labeled floating pill within the list view;
  keep it out of the active thread's action bar.
- Give thread reading a contextual Back control and only implemented actions.
  Separate messages with dividers and show a sender monogram, sender name,
  actual message date when available, and a native recipient disclosure.
  Render the message body on the open canvas, preserving safe Markdown.
- Dock a named Reply to thread action beneath the reader. Expand the existing
  composer when selected, preserving draft, revision and submission identity.
  Archived or busy threads must keep their actual capability limits.
- Label status truthfully. A Reply ready dot is a generation status, not an
  unread flag. Do not fabricate read tracking, inbox totals, message dates, or
  previews from a subject line. Omit unavailable metadata. Forge's Archive
  folder currently contains threads archived at the recall limit; a manual
  Archive control requires an implemented mutation contract.

Keep the compact global header and view navigation visible. Let the inbox or
thread body own scrolling so long content does not cut off the tabs or hide
the reply action. Preserve the desktop list-and-reader split and the phone's
single active pane. Check real generated states on both before accepting the
layout.

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

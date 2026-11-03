# Generated motion in a working IDE

Forge-specific adaptation, 2026-11-03, of Emil Kowalski's design-engineering,
animation-removal and performance guidance. See
[sources, changes and licenses](../../../../docs/design/skill-sources.md).
React, Motion, Expo and handwritten CSS/JS recipes are not imported.

## Decide whether motion helps

The workbench is used repeatedly. Keyboard focus, tree navigation, filtering,
selection and pane controls should respond immediately. Avoid replayed row
entrances, decorative hover scaling, pulsing chrome and transitions that delay
reading or repeat on every accepted view. Motion can clarify an occasional spatial
change or state transition, but it must have an identifiable user purpose.
Preserve a documented useful transition unless the requested scope covers changing
it; do not remove every animation indiscriminately.

Keep time-to-action separate from visual duration. Do not delay acceptance of a
typed event/result, input acknowledgement, focus restoration, admission rejection,
protected-DOM clearing or cleanup until an animation finishes. Loading feedback
must reflect the real request state; animated progress is not measured progress.

## Keep the Forge ownership split

- Declare reusable motion tokens and typed component behavior through supported
  YAML/Rust APIs. Inspect the schema/generator first. Unsupported duration, easing,
  transition or reduced-motion properties are a scoped generator gap, not an
  existing API and not a reason to add raw CSS/JS fields or a frontend dependency.
- Rust/WASM accepts domain intent/results and projects typed state. Main-thread
  presentation can apply generated browser transitions. Do not send every visual
  frame through WASM or put browser animation logic in the domain model.
- Keep generated adapters thin: bounded presentation updates, no second request
  reducer, hidden fetch path or untyped application controller. If a reusable
  generator primitive is needed and authorized, test it as generator code.

## Make retained motion safe

Prefer a short, interruptible transition over a long staged entrance. Choose exact
properties rather than a catch-all transition; prefer transform/opacity when they
preserve layout and readable content. Acceleration is browser-dependent, not a
guarantee. Do not trade correct geometry, text legibility or hit targets for a
compositor optimization. Measure expensive effects under representative report
size and main-thread load before adding layers or performance hints.

Respect the user's reduced-motion preference with an immediate or simpler result.
Final visibility, accessible state, focus, hit testing and cleanup must be correct
when duration is zero, the transition is interrupted or a completion event never
arrives. On removal, check hidden start styles, unmount timing and callbacks so
content does not remain invisible or wait for an animation that no longer runs.

Verify repeated/rapid input, reversal, keyboard use, narrow pane changes, reduced
motion, local cancellation, page leave/return and fatal clearing when affected.
Use real-browser evidence for perceived speed and frame cost; model/ABI tests only
prove their own layer. State what was measured versus inferred and name any
missing generator or browser coverage instead of claiming it already exists.

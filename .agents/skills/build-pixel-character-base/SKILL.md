---
name: build-pixel-character-base
description: Draw a pixel character as an editable greybox mannequin and pose it for a game. Use for sprite proportions, color-coded parts, idle and movement poses, pivots, or frame-specific hitboxes.
---

# Build a pixel character base

Use the [Homelab pixel-art tool](../../../src/python/pixel-art/README.md) for
integer-grid drawing recipes, sprite sheets, and cumulative drawing previews.
Start from the original scout recipe when the camera and scale fit.

1. Establish the camera, native cell size, facing direction, gameplay footprint,
   and ground/contact pivot. Use that pivot across the clip suite. At native
   size, block in the head, chest, and pelvis as distinct masses. Compare a
   two- or three-head-tall stylized character with a taller build if proportion
   is undecided. Pick the version whose pose reads at intended play size.
2. Split each bending limb at its joints. Give near and far limbs different
   greybox colors, parent each lower limb to its upper limb, and attach held
   props to a hand or forearm. Draw each segment around its own pivot with
   small part-local shapes. Set the rotation independently at each joint.
3. Block readable key poses before adding in-betweens. Keep feet planted as the
   hips lower into a crouch. For a jump, start with bent knees and arms behind
   the body, then extend through the legs and swing the arms up during push-off.
   Space airborne positions farther apart during fast ascent and descent and
   closer together near the apex. Bend the knees again to absorb contact.
   For a spin around the standing axis, draw front and back views with readable
   features and use the two profiles between them.
   For traversal, pose against a fixed test prop. Hold a hand on the ledge
   through the hang and first part of the pull, and keep one grip steady
   while the other moves on a climb. Check the foot contact at the end.
4. Attach a body box to the body and an action box to the moving part on active
   frames. Compare both to the artwork in canvas coordinates and check feet,
   reach, crop, and scale at native size. Use `steps` on a difficult pose to
   inspect exactly which operations build it.
5. Export the greybox PNG and manifest for playtesting. Treat canvas, pivot,
   frame timing, and hitboxes as a shared contract once approved. Record which
   clip or frame still needs review if the running game has no atlas loader.

When the mannequin and motion are approved, use
[finish-pixel-character-sprites](../finish-pixel-character-sprites/SKILL.md).

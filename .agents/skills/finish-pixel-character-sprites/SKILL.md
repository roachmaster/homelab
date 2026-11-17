---
name: finish-pixel-character-sprites
description: Add hand-drawn silhouettes, flat colors, shading, selective outlines, and interchangeable costume layers to an existing pixel character recipe. Use for finishing sprites or reskinning an approved mannequin and animation set.
---

# Finish a pixel character sprite

Use the [Homelab pixel-art tool](../../../src/python/pixel-art/README.md) to
edit draw operations and preview the existing poses. Export the greybox before
working on a supplied base so you can compare the result at native scale.
If the silhouette or timing still needs design, start with
[build-pixel-character-base](../build-pixel-character-base/SKILL.md).

1. Add `flat` layers to the approved parts. Test the silhouette in one color on
   the most extreme poses, then separate costume materials and near/far body
   parts into large readable clusters. Draw palette IDs into layer-local
   operations and keep the part anchors behind those operations.
2. Add `finish` layers for directional light and material detail. Pick one
   light direction for the clip suite. Use small value ramps per material and
   try a warmer light or cooler shadow where it improves separation. Prefer
   connected clusters over scattered single pixels. Use a dark edge where the
   silhouette needs separation and a colored edge where it catches light.
   Check against both bright and dark backgrounds.
3. Interleave each part's flat and finish layers where something in front must
   cover it. Put optional armor or cosmetics on layers tagged with a `skin`
   and attach them to the same parts. Test the untagged base and each variant
   on the strongest poses. A broader or taller silhouette can require a new
   gameplay review even when its drawing fits the cell.
4. Compare greybox, flat, and final at native size, then zoom using nearest
   neighbor to fix jagged edges or accidental gaps. Use `steps --layer` to
   isolate the construction of a particular layer over the layers below it.
   Verify the exported frame sizes, pivots, clip timing, and hitboxes match
   the approved greybox before handing off a costume.

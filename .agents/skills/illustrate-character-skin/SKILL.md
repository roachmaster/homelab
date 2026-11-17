---
name: illustrate-character-skin
description: Draw a clean illustrated 2D skin over approved character poses and prepare a 4× sprite atlas while preserving gameplay scale. Use for vector-style part art, joint seams, view and grip variants, or high-resolution atlas exports.
---

# Illustrate a 2D character skin

Start from an approved recipe made with
[build-pixel-character-base](../build-pixel-character-base/SKILL.md).
Use the [Room One Scout rig](../../../src/python/pixel-art/examples/room-one-scout.json)
and the [illustrated idle SVG](../../../src/python/pixel-art/examples/room-one-scout-illustrated-idle.svg)
as the pose and visual references. Keep the skeleton, part IDs, poses, views,
clip names, frame order and durations, logical pivot, and collision boxes as
the baseline. Keep the illustrated part drawings editable and separate from
the approved rig.

## Match the Scout's style

- **Silhouette and landmarks.** Match the idle proof at the same pivot and
  displayed size. Keep the helmet dome, visor, ear pod, pack, cyan probe,
  knee pad, and boot toe caps aligned with the approved pose. Use the chest
  patch, pack clasp, and forearm cuff as interior details.
- **Contour, seams, and rims.** At 4×, use 2.5–3 px navy-charcoal outer ink
  (`#181c28` or `#1e2436`) around the major masses. Use about 1.5 px muted
  mid-dark ink for interior details such as the cuff, pack flap, and knee
  pad. Add about 1.5 px of bright rim only along light-catching edges such
  as the shoulder cap, visor top bar, and boot sole.
- **Directional shading.** Build on the idle proof's broad, layered color
  shapes with a restrained top-left-to-bottom-right ramp inside each major
  material shape. Use golden yellow into warm amber for the helmet and slate
  blue into deep indigo for the suit. Keep material boundaries crisp. Give
  the glossy visor a crisp cyan specular bar and the helmet crown a short
  curved highlight. Check that the light direction reads consistently
  through the poses.

## Draw the skin

1. Draw the helmet, torso, upper and lower limbs, gloves, boots, and pack in
   part-local coordinates. Use curved contours, rounded shapes, and clear
   silhouette breaks. Center circular overlap shapes on the shoulder and
   knee pivots. Paint these underneath the adjoining pieces so the joint
   stays closed through its full bend.
2. Keep the diagonal strap and chest patch in the `torso` group behind
   `near_arm`. Keep the circular sleeve cap centered on the elbow and the
   cuff above the glove. Anchor the cyan `probe` to the hand socket on
   `near_forearm` so it travels as a rigid piece through walk, jump, and
   action poses. Check these overlaps at the intended in-game size.
3. Match the recipe's existing view tags and mirroring. The Scout uses an
   untagged side view, `front` and `back` for the spin, and a separate
   `ladder` rear view. Draw the chest strap across the front and the pack
   and visible harness on the rear. Check their occlusion as the spin
   passes through both profiles and as arms move in the ladder view.
   Prepare open, fist, and grab hands anchored to the same forearm sockets.
   Use per-hand state and prop visibility as presentation overrides.
   On contact frames, check where the drawn grip meets the ledge, rope, or rung.
4. Proof the extremes before finishing every clip. For the Scout, inspect
   zero-based `jump` frames 2 and 5 for overhead reach and knee tuck,
   `ledge_pull_up` frames 2 and 3 for catch and hang, both climbing clips
   for grip changes, and every `spin` view for silhouette and layering.
   Compare the art with the greybox at the same pivot and displayed size.

From the Homelab root, generate the knee-tuck drawing steps with the current tool.

```bash
python3 -B src/python/pixel-art/pixel_art.py steps \
  src/python/pixel-art/examples/room-one-scout.json \
  --stage greybox --clip jump --frame 5 --out src/python/pixel-art/out/knee-tuck
```

## Render and hand off

Use the [Homelab export command](../../../src/python/pixel-art/README.md)
with `--illustrated` and `--scale 4`. The
[Scout skin index](../../../src/python/pixel-art/examples/room-one-scout-illustrated.json)
maps views to their editable SVGs, selects alternate gloves for contact frames,
and stows the probe during the ledge climb.

- Compose joint positions and rotations in floating point through the full
  hierarchy for illustrated rendering. Scale to the working raster at draw
  time. Render each frame at 8× logical resolution, downsample it once with
  alpha-aware filtering to 4×, then pack it into the transparent atlas.
  Keep the approved 1× geometry path for pivots and hitboxes so smoother
  drawing preserves collision.
- For a scaled exporter, use a versioned scale contract. The Scout's output
  has `atlas_scale: 4`, `frame_size: [128, 176]` in atlas pixels, and a
  `logical_frame_size: [32, 44]`. Keep `pivot: [13, 41]` and hitboxes in
  logical units. An explicit pixel-space `atlas_pivot` is `[52, 164]`. Frame
  source rectangles use atlas pixels. In the Room One renderer, divide raster
  dimensions by the scale for drawing and place the sprite from the logical pivot.
- Compare every exported clip against the approved 1× manifest for frame
  counts, ordering, durations, pivots, and hitboxes. Open the atlas over light
  and dark backgrounds to check seams, clipping, and edge halos. Deliver the
  editable part art and pose proofs with the scaled PNG and manifest.

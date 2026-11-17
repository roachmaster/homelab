# Pixel-art authoring

Build a greybox mannequin and animation set as editable pixel operations. Add
silhouette, costume, and lighting layers when the poses are ready. The same
recipe exports each stage with shared frame geometry and timing.

The pixel renderer uses Python 3.12 and its standard library. Run commands
from the Homelab root.

```bash
tool=src/python/pixel-art/pixel_art.py
scout=src/python/pixel-art/examples/room-one-scout.json
out=src/python/pixel-art/out

python3 "$tool" check "$scout"
python3 "$tool" export "$scout" --stage greybox --out "$out/greybox"
python3 "$tool" steps "$scout" --stage greybox --clip crouch --frame 1 --out "$out/draw-steps"
python3 "$tool" export "$scout" --stage flat --out "$out/flat"
python3 "$tool" export "$scout" --stage final --skin ember --out "$out/ember"
python3 "$tool" export "$scout" --stage final --skin frost --out "$out/frost"
```

Open `steps.png` with nearest-neighbor zoom. Each cell adds the operation at the
same index in `steps.json`. Look up its layer and part in the recipe, edit one
coordinate or palette ID, then rerun the command to see where the change
first appears. Select a 0-based clip frame with `--frame` and an exact layer
name with `--layer` to focus on that layer's operations over the layers below.

## Edit a recipe

- `canvas` defines cell width, cell height, and a shared `pivot` in pixels.
  Coordinates increase right and down. Use the pivot for the in-game anchor
  and keep it stable across appearances.
- `palette` maps names to opaque `#RRGGBB` colors. Undrawn pixels are transparent.
- `parts` maps stable part IDs to an `offset` and an optional `parent`. A root's
  offset is in canvas coordinates. For children it is relative to the parent.
  Frame offsets add in parent space. A part's origin is its rotation pivot.
  Rotating a parent carries its descendants and their attached art along with it.
- `layers` draw back to front in array order. Each has a unique `name`, a
  `stage`, a `part`, and an ordered `draw` array. Each draw operation has a
  palette `color` and coordinates relative to that part.

  | Operation | Operands |
  | --- | --- |
  | `pixel` | `at: [x, y]` |
  | `rect` | `at: [x, y]`, `size: [w, h]` |
  | `line` | `from: [x, y]`, `to: [x, y]` |
  | `polygon` | `points: [[x, y], ...]` in perimeter order |

- Use the `greybox` stage alone to start. Add `flat` layers for silhouette and
  color blocking. The `final` view renders `flat` and `finish` together in
  source order. Untagged layers are shared. Tag a flat or finish layer with
  `skin` to draw it when that skin is selected.
- `clips` maps names to ordered lists of frames. A frame has `duration_ms`,
  optional `offsets` from part IDs to deltas, and optional `rotations` from part
  IDs to clockwise degrees relative to the parent. Use positive angles to turn
  the local right axis down. Each optional `hitboxes` entry has a `part`, an
  `at` point, and a `size`. Add an action box only to frames where it is active.
  Exported boxes are axis-aligned bounds after the part transform.
- A frame's optional `view` selects drawing layers tagged with that same view.
  Frames without a view use untagged layers. Use `flip_x: true` to mirror the
  selected view and its hitboxes around the shared horizontal pivot coordinate.

The recipe is plain JSON with `version: 1`. Check it after editing. The renderer
clips drawings at cell edges. Inspect extreme poses at native size to catch
cropping and check that the action box tracks the correct part.

## Export an illustrated skin

Use the [illustrated skin skill](../../../.agents/skills/illustrate-character-skin/SKILL.md)
to draw editable vector parts for the same rig. The Scout skin has side, front,
back, and ladder views, with alternate gloves for some contact frames. This
export uses `gdk-pixbuf-thumbnailer` with its SVG loader and ImageMagick
`convert` on the authoring machine.

```bash
python3 -B src/python/pixel-art/pixel_art.py export \
  src/python/pixel-art/examples/room-one-scout.json \
  --illustrated src/python/pixel-art/examples/room-one-scout-illustrated.json \
  --scale 4 --out src/python/pixel-art/out/illustrated
```

Each skin view points to an SVG with the recipe's logical canvas as its view
box. Use a single drawing group containing the named part groups in paint
order, and keep each part's drawing in its own joint coordinates. The
exporter places the group at the animated joint, so the top-level part
transform can show a neutral preview when the SVG is opened directly.
The optional `grips` map lists which part IDs switch to their
`data-grip="grab"` art at each zero-based frame. The `hidden` map omits
part artwork at selected frames, as when the Scout stows the probe while
climbing the ledge.

### Study the scout's key poses

- In `crouch`, the hips lower, the knees bend forward, and the shins angle
  back toward planted feet. Inspect how each leg is drawn around its joints,
  then compare the shoulder and elbow angles to the standing pose.
- In `jump`, watch the arms swing back during compression and drive forward
  and upward as the legs extend. The airtime samples follow a gravity-shaped
  arc near the fixed ground pivot. The knees tuck near the top, extend toward
  contact, and bend again to absorb the landing.
- In `spin`, compare the right profile, front, mirrored left profile, and back.
  Follow the visor and pack to read the turn. Front and back have their own
  drawings so shoulder width and visible surfaces change as the scout turns.
- Compare the silhouettes at the active frames of `punch`, `uppercut`, `kick`,
  and `roundhouse_kick`. The action box follows the animated forearm or boot
  during those frames. The original `attack` clip uses the scout's probe.
- In `roll`, follow the pack and helmet through the tuck and rotation. The
  body box compresses during the roll. The clip stays anchored in place so
  the game can control travel independently.
- In `ledge_pull_up`, follow the hands from the jump to the edge and through
  the hang. Then follow the knee and foot as weight shifts onto the platform.
- `rope_climb` braces against a single rope. `ladder_climb` alternates grips and
  steps across rungs. Use a fixed prop in the preview so sliding contact is
  visible when timing or placement changes.
- `barrel_vault` reaches across the top of the barrel and tucks past it.
  `barrel_jump_off` starts with the feet on the barrel and ends on the floor.

## Use the export in a game

Read `manifest.json` version 1 next to its transparent `sheet.png`. Use each
frame's `rect` as its `[x, y, width, height]` source rectangle in the atlas.
Advance frames using the selected clip's `duration_ms` values in array order.
To draw at an actor anchor, subtract the shared pivot from the actor's position
to place the destination cell. Hitboxes are `[x, y, width, height]` in that
cell's local coordinate space. Apply the same top-left offset to position them.
The manifest also includes the stage, selected skin, and fixed frame size.
For a given recipe the frame rectangles, durations, pivot, and hitboxes are
identical across 1× stages and skins.

Illustrated atlases use manifest version 2. The Scout's `frame_size` is
`[128, 176]` atlas pixels at `atlas_scale: 4`. Its `logical_frame_size` is
`[32, 44]`. Hitboxes and `pivot` remain in logical units, and the separate
`atlas_pivot` gives the pivot in atlas pixels. Use the atlas-pixel source
rectangle and divide its dimensions by `atlas_scale` for the destination.

The Room One browser renderer currently draws primitive shapes and has no atlas
loader. These files are an integration input for that renderer or another
consumer. The recipe format is specific to this tool.

The scout's jump preview bakes height into the frame's `root` offset. For a
game that applies its own vertical motion, set the airborne `root` offsets to
`[0, 0]` before export. Keep knee and arm rotations for the animation and use
the runtime trajectory for world position.

Keep walls, ropes, ladders, and barrels in the level. The scout atlas carries
only the character. Align the ledge catch to the platform corner, advance the
climb loops along the prop, and move the actor horizontally through the barrel
clips. Check the hand or foot contact against the level geometry at native size.
The sample uses a ledge corner at `(22, 27)` and a rope at `x = 19` in the
actor's starting cell. Its ladder rungs are six pixels apart. The barrel top
sits 12 pixels above the floor.

## Tests

```bash
python3 -B -m unittest discover -s src/python/pixel-art/tests -v
```

There is no build step. Illustrated export tests use the same image tools as
the illustrated CLI.

## Drawing references

- Study proportions, silhouette blocking, and palette ramps in Pixel Art
  Studio. Its LPC workflow is a reference for swappable gear on a shared body.
  [Character patterns](https://github.com/Gamezxz/pixel-art-studio/blob/f8c246635c4621a6c2b427833149afdc3dc3c719/references/patterns.md) · [Drawing techniques](https://github.com/Gamezxz/pixel-art-studio/blob/f8c246635c4621a6c2b427833149afdc3dc3c719/references/techniques.md) · [LPC layout](https://github.com/Gamezxz/pixel-art-studio/blob/f8c246635c4621a6c2b427833149afdc3dc3c719/references/lpc.md)
- Study semantic parts, integer pose changes, and sparse pixel edits in Code
  as Pixel Art. [Code as Pixel Art skill](https://github.com/dakotacsk/code-as-pixelart/blob/7acd7c9cf0a1d1eb540fa9c85d8c74adda0952e4/skills/code-as-pixelart/SKILL.md)
- Study how Pixelbolt builds a final image through inspectable drawing
  operations. [Pixelbolt skill](https://github.com/skulitom/Pixelbolt/blob/52a5767e0b2594eb772f197839a1d3f54b77fd20/skills/pixel-art/SKILL.md) · [Greybox reel](https://github.com/skulitom/Pixelbolt/blob/52a5767e0b2594eb772f197839a1d3f54b77fd20/showcase/greybox-reel/README.md)

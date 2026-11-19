#!/usr/bin/env python3
"""Render the existing Scout recipe as fixed portal showcase assets.

This is an art export, not application HTML/CSS/JS or a new character design.
It reuses the approved pixel renderer, SVG part assembly and supersampled export.
GIF previews quantize source timings to centiseconds cumulatively; the exported
atlas manifest retains exact durations, logical pivot and collision boxes.
"""
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
from tempfile import TemporaryDirectory
from xml.etree import ElementTree as ET

ROOT = Path(__file__).resolve().parents[5]
TOOLS = ROOT / 'src/python/pixel-art'
sys.path.insert(0, str(TOOLS))
from pixel_art import Raster, rgba_palette
from recipe import frame_hitboxes, frame_poses, read_recipe, visible_layers
from illustrated import export_illustrated, paint_frame, read_views, render_sheet, svg_tag

RECIPE = TOOLS / 'examples/room-one-scout.json'
SKIN = TOOLS / 'examples/room-one-scout-illustrated.json'
VIEWS = ('idle', 'front', 'back', 'ladder')
BACKGROUND = '#121721'


def pixel_idle(recipe, stage, destination):
    canvas = recipe['canvas']
    image = Raster(canvas['width'], canvas['height'])
    frame = recipe['clips']['idle'][0]
    poses = frame_poses(recipe, frame)
    palette = rgba_palette(recipe)
    for layer in visible_layers(recipe, stage, None, frame.get('view')):
        for operation in layer['draw']:
            image.draw(operation, poses[layer['part']], palette)
    image.write_png(destination)


def source_manifest(recipe, illustrated):
    assert illustrated['version'] == 2 and illustrated['atlas_scale'] == 4
    assert illustrated['logical_frame_size'] == [32, 44]
    assert illustrated['frame_size'] == [128, 176]
    assert illustrated['pivot'] == recipe['canvas']['pivot'] == [13, 41]
    assert illustrated['atlas_pivot'] == [52, 164]
    assert list(illustrated['clips']) == list(recipe['clips'])
    for clip, frames in recipe['clips'].items():
        exported = illustrated['clips'][clip]
        assert len(exported) == len(frames)
        for original, rendered in zip(frames, exported):
            assert original['duration_ms'] == rendered['duration_ms']
            assert frame_hitboxes(original, frame_poses(recipe, original)) == rendered['hitboxes']


def gif_delays(frames):
    # Bound cumulative timing drift, rather than rounding each frame independently.
    elapsed = previous = 0
    delays = []
    for frame in frames:
        elapsed += frame['duration_ms']
        rounded = (elapsed + 5) // 10
        delays.append(rounded - previous)
        previous = rounded
    assert all(delay >= 1 for delay in delays)
    return delays


def ribbon(recipe, clip, frames, destination, directory):
    width, height = recipe['canvas']['width'], recipe['canvas']['height']
    logical = (width * len(frames), height)
    scaled = tuple(value * 4 for value in logical)
    working = tuple(value * 2 for value in scaled)
    sheet = ET.Element(svg_tag('svg'), {
        'width': str(working[0]), 'height': str(working[1]),
        'viewBox': f'0 0 {logical[0]} {logical[1]}',
    })
    views, grips, hidden = read_views(SKIN, recipe, ET.SubElement(sheet, svg_tag('defs')))
    for index, frame in enumerate(frames):
        paint_frame(sheet, (index * width, 0), (width, height),
                    views[frame.get('view', 'side')], frame_poses(recipe, frame, snap=False),
                    grips.get((clip, index), set()), hidden.get((clip, index), set()))
    source = directory / f'{clip}.svg'
    ET.ElementTree(sheet).write(source, encoding='utf-8')
    render_sheet(source, directory / f'{clip}-working.png', destination, working, scaled)
    return scaled


def animated_preview(clip, frames, strip, target, directory):
    convert = shutil.which('magick') or shutil.which('convert')
    if not convert:
        raise SystemExit('Existing ImageMagick is required; this exporter installs nothing')
    pattern = directory / f'{clip}-frame-%02d.png'
    subprocess.run([convert, str(strip), '-crop', '128x176', '+repage', str(pattern)], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    delays = gif_delays(frames)
    command = [convert]
    for index, delay in enumerate(delays):
        command.extend(['-delay', str(delay), str(directory / f'{clip}-frame-{index:02d}.png')])
    command.extend(['-background', BACKGROUND, '-alpha', 'remove', '-alpha', 'off',
                    '-dispose', 'none', '-loop', '0', '-strip', '-layers', 'Optimize', str(target)])
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    return delays


def png_dimensions(path):
    data = path.read_bytes()
    assert data.startswith(b'\x89PNG\r\n\x1a\n')
    return struct.unpack('>II', data[16:24])


def main():
    output = Path(__file__).resolve().parent
    (output / 'clips').mkdir(exist_ok=True)
    recipe = read_recipe(RECIPE)
    assert len(recipe['clips']) == 16
    assert sum(map(len, recipe['clips'].values())) == 103
    pixel_idle(recipe, 'greybox', output / 'mannequin-idle.png')
    pixel_idle(recipe, 'final', output / 'pixel-idle.png')
    for view in VIEWS:
        shutil.copyfile(TOOLS / f'examples/room-one-scout-illustrated-{view}.svg',
                        output / f'scout-illustrated-{view}.svg')
    with TemporaryDirectory(prefix='scout-showcase-') as temporary:
        directory = Path(temporary)
        atlas = directory / 'atlas'
        export_illustrated(recipe, SKIN, 4, atlas)
        manifest = json.loads((atlas / 'manifest.json').read_text())
        source_manifest(recipe, manifest)
        manifest['image'] = 'illustrated-sheet.png'
        shutil.copyfile(atlas / 'sheet.png', output / manifest['image'])
        (output / 'illustrated-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        clips = []
        for name, frames in recipe['clips'].items():
            png = output / 'clips' / f'{name}.png'
            gif = output / 'clips' / f'{name}.gif'
            size = ribbon(recipe, name, frames, png, directory)
            assert png_dimensions(png) == size
            delays = animated_preview(name, frames, png, gif, directory)
            clips.append({'id': name, 'frame_count': len(frames),
                          'duration_ms': sum(frame['duration_ms'] for frame in frames),
                          'frame_durations_ms': [frame['duration_ms'] for frame in frames],
                          'gif_delays_centiseconds': delays,
                          'strip': f'clips/{name}.png', 'motion_preview': f'clips/{name}.gif',
                          'strip_size': list(size)})
    sources = [RECIPE, SKIN] + [TOOLS / f'examples/room-one-scout-illustrated-{view}.svg' for view in VIEWS]
    report = {
        'schema_version': 1, 'logical_frame_size': [32, 44], 'atlas_frame_size': [128, 176],
        'atlas_scale': 4, 'pivot': [13, 41], 'atlas_pivot': [52, 164],
        'clip_count': 16, 'frame_count': 103,
        'gif_preview': {'background': BACKGROUND, 'timing': 'cumulative centisecond rounding; at most 5 ms timeline error',
                        'note': 'Motion preview only; PNG/SVG and the atlas manifest are the authoritative art/timing contract.'},
        'sources': {path.relative_to(ROOT).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources},
        'clips': clips,
    }
    (output / 'showcase-manifest.json').write_text(json.dumps(report, indent=2) + '\n')
    print(f'Exported {len(clips)} existing Scout clips ({report["frame_count"]} frames)')


if __name__ == '__main__':
    main()

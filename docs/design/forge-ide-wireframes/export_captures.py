#!/usr/bin/env python3
"""Export reviewed, fixed-name owned QA captures as the portal's design references.

This copies screenshots unchanged; it never edits pixels or captures live operator data.
Run only after inspecting the chosen fixture images from a passed make ui-qa run.
"""
import datetime
import hashlib
import json
from pathlib import Path
import shutil
import struct
import sys

ROOT = Path(__file__).resolve().parents[3]
FILES = {
    'forge-ide-chat-phone.png': 'ide-chat-phone.png',
    'forge-ide-chat-desktop.png': 'ide-chat-desktop.png',
    'forge-ide-chat-history-phone.png': 'ide-chat-history-phone.png',
    'forge-ide-workbench-phone.png': 'ide-viewport-iphone13-portrait.png',
    'forge-ide-workbench-explorer-phone.png': 'ide-workbench-explorer-phone.png',
    'forge-ide-workbench-project-options.png': 'ide-workbench-project-options-phone.png',
    'forge-ide-workbench-desktop.png': 'ide-workbench-desktop.png',
    'forge-ide-mail-inbox-phone.png': 'ide-mail-inbox-phone.png',
    'forge-ide-mail-thread-phone.png': 'ide-mail-thread-phone.png',
    'forge-ide-mail-thread-desktop.png': 'ide-mail-thread-desktop.png',
}


def main():
    if len(sys.argv) != 2:
        raise SystemExit('usage: export_captures.py PASSED_QA_RUN_DIRECTORY')
    run = Path(sys.argv[1]).resolve(strict=True)
    metadata = json.loads((run / 'run.json').read_text())
    browser = json.loads((run / 'browser/result.json').read_text())
    if metadata['status'] != 'passed' or metadata['source_consistency'] != 'unchanged' or browser['status'] != 'passed':
        raise SystemExit('Only a passed unchanged-source owned fixture run can be exported')
    records = []
    for target, fixture in FILES.items():
        path = run / 'browser' / fixture
        if path.is_symlink():
            raise SystemExit('Capture must be a regular PNG')
        data = path.read_bytes()
        if not data.startswith(b'\x89PNG\r\n\x1a\n') or not 24 <= len(data) <= 2 * 1024 * 1024:
            raise SystemExit('Capture is not a bounded PNG')
        width, height = struct.unpack('>II', data[16:24])
        records.append({'asset': target, 'fixture_capture': fixture, 'width': width, 'height': height,
                        'sha256': hashlib.sha256(data).hexdigest()})
    destination = ROOT / 'src/rust/forge-ide-web-host/design'
    for record in records:
        shutil.copyfile(run / 'browser' / record['fixture_capture'], destination / record['asset'])
    provenance = {'schema_version': 1, 'reviewed_on': datetime.datetime.now(datetime.timezone.utc).date().isoformat(),
                  'scope': 'Disposable generated-UI acceptance fixtures; editable controls masked; no operator data.',
                  'source': {key: metadata['source'][key] for key in ('revision', 'dirty', 'source_sha256') if key in metadata['source']}, 'captures': records}
    (Path(__file__).parent / 'capture-manifest.json').write_text(json.dumps(provenance, indent=2) + '\n')
    print(f'Exported {len(records)} reviewed fixture captures')


if __name__ == '__main__':
    main()

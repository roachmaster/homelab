#!/usr/bin/env python3
"""Build the Forge IDE UI architecture report from canonical Mermaid SVGs.

Requires Python 3.9+, reportlab, PyMuPDF, and Inkscape on PATH.
Run from anywhere: python3 docs/architecture/proposals/build_forge_ide_ui_pdf.py
The committed SVGs are inputs; regenerate them with the gallery's Mermaid tooling.
"""
from __future__ import annotations
import argparse
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET

import fitz
from reportlab.lib.colors import HexColor, Color, white
from reportlab.lib.styles import ParagraphStyle
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas
from reportlab.platypus import Paragraph

ROOT = Path(__file__).resolve().parents[3]
BASE = '175315b4fe1c993f15bb2ff52d824ee4cdaecc88'
GH = 'https://github.com/roachmaster/homelab'
PROPOSAL = GH + '/blob/main/docs/architecture/proposals/forge-ide-ui.md'
GALLERY = GH + '/tree/main/docs/architecture/diagrams/mermaid'
SKILL = GH + '/blob/' + BASE + '/.agents/skills/design-forge-front-door/SKILL.md'
W, H = 792, 612
M = 36
INK = HexColor('#122331')
MUTED = HexColor('#526573')
LINE = HexColor('#D5E1E6')
BG = HexColor('#F4F8FA')
TEAL = HexColor('#176C68')
TEAL_PALE = HexColor('#E9F6F2')
BLUE = HexColor('#2B5C8A')
BLUE_PALE = HexColor('#EEF4FB')
AMBER = HexColor('#916221')
AMBER_PALE = HexColor('#FFF5E4')
FONT = 'Helvetica'
BOLD = 'Helvetica-Bold'
MONO = 'Courier'
FONT_PATHS = [Path('/usr/share/fonts/truetype/dejavu'), Path('/Library/Fonts')]
for fp in FONT_PATHS:
    if (fp / 'DejaVuSans.ttf').exists() and (fp / 'DejaVuSans-Bold.ttf').exists():
        pdfmetrics.registerFont(TTFont('ForgeSans', str(fp / 'DejaVuSans.ttf')))
        pdfmetrics.registerFont(TTFont('ForgeBold', str(fp / 'DejaVuSans-Bold.ttf')))
        FONT, BOLD = 'ForgeSans', 'ForgeBold'
        pdfmetrics.registerFontFamily('ForgeSans', normal='ForgeSans', bold='ForgeBold', italic='ForgeSans', boldItalic='ForgeBold')
        break

NAV = [('Overview', 'overview'), ('Layout', 'layout'), ('Build', 'build'), ('Request', 'request'),
       ('State', 'state'), ('Ownership', 'ownership'), ('Security', 'security'), ('Plan', 'plan')]
DIAGRAMS = ['m19-ui-layout', 'm20-ui-build-runtime', 'm21-ui-request', 'm22-ui-state', 'm24-ui-contracts', 'm23-ui-trust']
DIAGRAM_PAGES = [1, 2, 3, 4, 5, 6]
WORKERS = 'https://developer.mozilla.org/en-US/docs/Web/API/Web_Workers_API/Using_web_workers'
MEMORY = 'https://developer.mozilla.org/en-US/docs/WebAssembly/Reference/JavaScript_interface/Memory/grow'
CSRF = 'https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html'


def source(path):
    return GH + '/blob/' + BASE + '/' + path


def para(c, text, x, top, width, size=10, leading=None, color=INK, bold=False):
    style = ParagraphStyle('body', fontName=BOLD if bold else FONT, fontSize=size,
                           leading=leading or size * 1.43, textColor=color,
                           spaceBefore=0, spaceAfter=0)
    p = Paragraph(text, style)
    _, h = p.wrap(width, 1000)
    p.drawOn(c, x, top-h)
    return h


def label(c, text, x, y, size=8, color=MUTED, bold=False):
    c.setFillColor(color)
    c.setFont(BOLD if bold else FONT, size)
    c.drawString(x, y, text)


def link(c, title, url, x, y, size=8.1, color=BLUE):
    label(c, title, x, y, size, color)
    tw=pdfmetrics.stringWidth(title, FONT, size)
    c.linkURL(url, (x, y-2, x+tw, y+size+2), relative=0, thickness=0)
    return tw


def chip(c, text, x, y, fill, textcolor, w=None):
    width = w or pdfmetrics.stringWidth(text, BOLD, 7.7)+18
    c.setFillColor(fill)
    c.roundRect(x, y-5, width, 17, 5, stroke=0, fill=1)
    label(c,text,x+9,y,7.7,textcolor,True)
    return width


def base_page(c, number, key, title, intro):
    c.setFillColor(white); c.rect(0,0,W,H,stroke=0,fill=1)
    c.bookmarkPage(key)
    c.addOutlineEntry(title, key, level=0)
    label(c, 'HOMELAB  /  UI ARCHITECTURE REPORT', M, 580, 8.3, MUTED, True)
    c.setFont(FONT,8.3); c.setFillColor(MUTED)
    c.drawRightString(W-M,580,'DESIGN PROPOSAL  /  02 NOV 2026')
    label(c,title,M,545,23,INK,True)
    para(c,intro,M,525,W-2*M,10.3,14.1)
    # The diagrams' own styling distinguishes implemented and proposed nodes.
    c.setFillColor(LINE); c.rect(M,57,W-2*M,.6,stroke=0,fill=1)
    x=M
    for name,dest in NAV:
        color=INK if dest == key else MUTED
        label(c,name,x,36,8.1,color,dest==key)
        tw=pdfmetrics.stringWidth(name,BOLD if dest==key else FONT,8.1)
        c.linkRect('',dest,(x,33,x+tw,47),relative=0,thickness=0)
        x+=tw+18
    c.setFillColor(MUTED);c.setFont(FONT,8)
    c.drawRightString(W-M,36,f'{number} / 8')
    link(c,'GitHub',GH,M,15,7.2)
    link(c,'Proposal',PROPOSAL,M+45,15,7.2)
    link(c,'Diagram gallery',GALLERY,M+96,15,7.2)
    link(c,'Front door skill',SKILL,M+178,15,7.2)
    c.setFillColor(MUTED);c.setFont(FONT,7.1)
    c.drawRightString(W-M,15,'Baseline '+BASE[:7]+'  |  Browser UI and host are proposed not implemented')


def note(c,title,text,x,top,width=340,color=TEAL):
    label(c,title,x,top,9.4,color,True)
    para(c,text,x,top-10,width,9.1,12.5)


def evidence(c,items,y=72):
    x=M
    for title,url in items:
        width=link(c,title,url,x,y,7.7)
        x+=width+24


def diagram_box(c, index):
    url = GH + '/blob/main/docs/architecture/diagrams/mermaid/images/' + DIAGRAMS[index] + '.svg'
    link(c, 'Open full size SVG', url, W-M-78, 480, 7.1)
    c.setFillColor(BG)
    c.roundRect(M,171,W-2*M,304,8,stroke=0,fill=1)


def panel(c,x,y,w,h,title,body,color=TEAL,fill=TEAL_PALE):
    c.setFillColor(fill);c.roundRect(x,y,w,h,7,stroke=0,fill=1)
    para(c,title,x+14,y+h-15,w-28,11.2,14,color,True)
    para(c,body,x+14,y+h-43,w-28,9.3,13.1)


def create_base(path):
    c=canvas.Canvas(str(path),pagesize=(W,H),pageCompression=1,invariant=1)
    c.setTitle('Forge IDE UI Architecture Report')
    c.setAuthor('Homelab architecture')
    c.setCreator('Homelab UI report builder')
    c.setSubject('Rust owned browser state with a stable generated shell and a scoped native host')

    base_page(c,1,'overview','Forge IDE UI Architecture Report',
        'Recommendation: add a Rust owned browser UI to the existing read only agent. '
        'Use a stable generated shell, a small browser adapter and a same origin native host; do not replace the architecture with React.')
    panel(c,M,330,347,137,'Implemented foundation',
          '<b>forge-agent</b> selects named nodes and permitted project aliases. Its mTLS gateway invokes '
          'the shared bridge and returns a typed structural <b>ProjectReport</b>.<br/><br/>'
          'Localhost two-process evidence is not a production deployment.',TEAL,TEAL_PALE)
    panel(c,409,330,347,137,'Proposed UI addition',
          '<b>Rust WASM</b> owns intent, request state and rendering decisions. A native UI host keeps '
          'agent credentials off the browser and projects reports into a portable <b>ReportView</b>.<br/><br/>'
          'No browser UI or browser identity setup exists yet.',AMBER,AMBER_PALE)
    label(c,'First experience to build',M,305,12.2,INK,True)
    stages=[('SELECT','Allowed node and project'),('INSPECT','One bounded request'),('EXPLORE','Tree and Rust declarations'),('UNDERSTAND','Completeness and failures')]
    for i,(title,body) in enumerate(stages):
        x=M+i*183
        c.setFillColor(BG);c.roundRect(x,222,171,63,5,stroke=0,fill=1)
        label(c,title,x+11,264,8.4,TEAL,True)
        para(c,body,x+11,252,149,9.1,12.6)
    note(c,'Use the references as a guide',
         'Mission Control informs the prepare, worker fetch, response acceptance and render loop. '
         'Reuse the ownership pattern, not private source code or assumptions that its generator is drop-in.',M,192)
    note(c,'Keep the first UI honest',
         'Show a structural report, not a source editor, semantic compiler or live filesystem. '
         'No file-content endpoint, watcher, remote jobs or mutation API is part of this report.',410,192)
    para(c,'<b>Decision to make before implementation</b>  Approve the Rust UI path, a scoped single-user host trust model, '
           'and the browser contract. This document does not authorize deployment or credential provisioning.',M,117,W-2*M,9.2,12.6,MUTED)
    evidence(c,[('Implemented agent',source('src/rust/forge-agent/README.md')),
                ('Current report model',source('src/rust/forge-ide/src/model.rs')),
                ('Full UI proposal',PROPOSAL)])
    c.showPage()

    base_page(c,2,'layout','Add UI packages without reversing dependencies',
        'Separate portable UI policy, the browser boundary and the native host. All new package names below '
        'are proposals. Existing agent, capability and core contracts stay independently usable.')
    diagram_box(c,0)
    note(c,'Do not import the native capability into WASM',
         '<b>forge-ide</b> inspects trusted projects with native filesystem and parser dependencies. The host validates '
         '<b>ProjectReport</b> and maps it to a bounded portable <b>ReportView</b>; a WASM build is a separate gate.',M,150)
    note(c,'Preserve one command front door',
         'Static assets use GET. Commands share <b>POST /forge/v1/dispatch</b>. A bounded native owner runs '
         'the synchronous remote task and HTTPS objects; never call them on arbitrary async threads.',410,150)
    evidence(c,[('Existing composition',source('src/rust/forge-agent/README.md')),
                ('Typed remote task',source('src/rust/forge-ide/src/remote.rs')),
                ('Front door invariant',SKILL)])
    c.showPage()

    base_page(c,3,'build','Keep build outputs separate from live state',
        'Generate the stable HTML CSS and JavaScript shell once per build. Compile Rust UI logic to WASM. '
        'At runtime, load those assets and update selected DOM regions instead of regenerating the application.')
    diagram_box(c,1)
    panel(c,M+15,207,335,112,'Native build only',
          'Publish asset hashes, protocol version and expected WASM exports in a manifest. '
          'Compile a real module and test the emitted bundle. Worker and DOM modules remain authored source.',BLUE,BLUE_PALE)
    panel(c,M+370,207,335,112,'Browser runtime only',
          'Apply typed views to stable mount regions and preserve focus and expansion. '
          'Asset serving reads the bundle; it does not invoke the template factory for each response.',TEAL,TEAL_PALE)
    note(c,'Reference tools are not interchangeable',
         'Standalone UI references have separate static website and raw-ABI WASM paths. Mission Control '
         'uses an embedded legacy generator; it does not prove external codegen is already integrated.',M,150)
    note(c,'Choose a small generation contract',
         'Whole-file code generation is not <b>forge-build</b> OUT_DIR resource embedding. Validate schema '
         'and template compatibility; keep generated files separate from handwritten state and effect logic.',410,150)
    evidence(c,[('Reference interpretation and build decision',PROPOSAL),
                ('Current homelab template API',source('src/rust/forge-template/README.md'))])
    c.showPage()

    base_page(c,4,'request','Route asynchronous effects through typed boundaries',
        'One worker owns the Rust WASM model and fetch. It executes typed effects and lets Rust accept replies '
        'before publishing views. The native host separately admits browser sessions and invokes the agent.')
    diagram_box(c,2)
    note(c,'Host admission is not agent admission',
         'Host checks session, Host/Origin, CSRF and allowed selection. Its shared bridge invokes '
         '<b>RemoteInspectTask</b> with configured endpoint and budget; the browser supplies neither an arbitrary URL nor a path.',M,150)
    note(c,'The agent checks authority again',
         'Agent checks mTLS subject, operation and project; its shared bridge guards inspection. Decode even '
         'HTTP 200 as report or error. Validate bounded <b>ReportView</b> and token before Rust publishes targeted updates.',410,150)
    evidence(c,[('Remote request and reply checks',source('src/rust/forge-ide/src/remote.rs')),
                ('Agent dispatch policy',source('src/rust/forge-agent/src/dispatch.rs')),
                ('Worker boundary',WORKERS)])
    c.showPage()

    base_page(c,5,'state','Make stale responses unable to replace current state',
        'Treat selected node and project, the active request and accepted report as explicit Rust state. '
        'A response must match the session epoch, request ID and selection generation before changing the view.')
    diagram_box(c,3)
    note(c,'A successful request can yield a partial report',
         'Distinguish loading, ready, incomplete, denied, unavailable and invalid response. Preserve '
         '<b>complete=false</b>, excluded entries and diagnostics; do not display a green success badge for missing analysis.',M,150)
    note(c,'Cancellation is a UI decision first',
         'A new selection invalidates old work and its visible context. Abort fetch where possible, but '
         'ignore late replies regardless. Browser abort does not prove the agent stopped synchronous inspection.',410,150)
    evidence(c,[('Report completeness semantics',source('src/rust/forge-ide/src/model.rs')),
                ('Agent timeout and shutdown limits',source('src/rust/forge-agent/README.md'))])
    c.showPage()

    base_page(c,6,'ownership','Make state and resource ownership explicit',
        'The worker owns the WASM instance, Rust view model and fetch. The small main-thread adapter applies '
        'DOM updates. Neither generated markup nor a second JavaScript store becomes the source of truth.')
    diagram_box(c,4)
    note(c,'Mount and dispose deliberately',
         'Mount once, attach delegated listeners and start bounded worker activity. On unmount, invalidate '
         'the generation, abort owned fetches, remove listeners and release handles. Workers never touch the DOM directly.',M,150)
    note(c,'A raw ABI needs a tested ownership contract',
         'Version messages; define allocation, lengths and release. Copy bounded buffers before asynchronous '
         'use, refresh views after WASM memory growth, and never keep borrowed pointers across an await.',410,150)
    evidence(c,[('Web Worker ownership',WORKERS),('WASM memory growth',MEMORY),('Proposed interface contracts',PROPOSAL)])
    c.showPage()

    base_page(c,7,'security','Keep browser trust separate from agent trust',
        'Recommend a same origin host bound to explicit loopback addresses for a trusted single-user first slice. '
        'Its protected session bootstrap is new work; mTLS keys never enter the browser.')
    diagram_box(c,5)
    security_cards=[
        ('Browser session','Protected bootstrap supplies only allowed node and project keys. '
         'A header or view label is not authority.'),
        ('Native host','Effective grants intersect session, host configuration and agent service grants. '
         'No arbitrary relay or secret-bearing assets.'),
        ('Agent service identity','The agent sees the host TLS subject, not the browser user. '
         'Its operation and project policy still applies.'),
    ]
    for i,(title,body) in enumerate(security_cards):
        x=M+15+i*232
        c.setFillColor(AMBER_PALE if i<2 else TEAL_PALE);c.roundRect(x,216,222,105,6,stroke=0,fill=1)
        para(c,title,x+11,305,200,10.3,13.1,AMBER if i<2 else TEAL,True)
        para(c,body,x+11,280,200,8.8,12.5)
    note(c,'The host must not be an unrestricted proxy',
         'Allowlist session, operation, node and project before forwarding. Use a narrowly scoped service '
         'identity and preserve agent checks. This is not end-user delegation; multi-user isolation needs a separate design.',M,150)
    note(c,'Browser protections remain mandatory',
         'Require session authentication, expected Host/Origin and CSRF checks, secure cookie policy and no permissive '
         'credentialed CORS. Render text safely, test CSP, and keep reports and secrets out of logs and persistent browser storage.',410,150)
    evidence(c,[('Current agent authority boundary',source('src/rust/forge-agent/README.md')),
                ('CSRF and Origin guidance',CSRF),('Trust model and open decisions',PROPOSAL)])
    c.showPage()

    base_page(c,8,'plan','Ship a narrow browser inspection slice',
        'Build the smallest useful path from an allowed project selection to a validated structural report. '
        'Each stage needs tests for rejection, stale data and cleanup before expanding the surface area.')
    stages=[('01','Pure state and view','Portable model and fixture tests',TEAL,TEAL_PALE),
            ('02','Generated shell','Stable mounts and ABI smoke test',TEAL,TEAL_PALE),
            ('03','Scoped native host','Session policy and agent adapter',BLUE,BLUE_PALE),
            ('04','Browser integration','Real interaction and failure tests',BLUE,BLUE_PALE)]
    for i,(n,title,detail,color,fill) in enumerate(stages):
        x=M+i*183
        c.setFillColor(fill);c.roundRect(x,376,171,97,7,stroke=0,fill=1)
        label(c,n,x+12,451,13,color,True)
        para(c,title,x+12,439,147,10.7,13.2,color,True)
        para(c,detail,x+12,406,147,8.5,11.2,color)
    label(c,'Acceptance evidence required',M,349,12.5,INK,True)
    rows=[
      ('Portable boundary','WASM builds without native filesystem TLS or parser crates; default core profiles stay unchanged.'),
      ('Generator and ABI','Deterministic outputs, version mismatch refusal, balanced buffer release and mount cleanup.'),
      ('State and rendering','Out-of-order and duplicate replies, selection changes, partial reports, escaping and bounded DOM work.'),
      ('Authority and transport','Unauthenticated or cross-origin request, denied node/project, forged identity, wrong peer and oversized reply.'),
      ('User experience','Keyboard tree navigation, visible focus, accessible status, empty/error states and slow network behavior.'),
      ('End to end','Real localhost browser / host / agent flow with ephemeral identities; late replies never repaint.')]
    y=326
    for i,(left,right) in enumerate(rows):
        c.setFillColor(BG if i%2==0 else white);c.rect(M,y-33,W-2*M,33,stroke=0,fill=1)
        para(c,left,M+10,y-8,161,9.1,11.8,INK,True)
        para(c,right,M+184,y-8,524,9.0,11.8)
        y-=33
    para(c,'<b>Deliberately later</b>  Source text editing, writes, language-server features, remote jobs, live subscriptions, '
           'multi-user delegation and alternate carriers. No runtime code, deployment or credential provisioning is delivered by this report.',M,112,W-2*M,8.9,12.1,MUTED)
    evidence(c,[('Full proposal and decision gates',PROPOSAL),('Rebuild this report',GH+'/blob/main/docs/architecture/proposals/forge-ide-ui-pdf.md')],y=70)
    c.showPage();c.save()


def inkscape_compatible_svg(source_svg, destination_svg):
    """Normalize CSS rgba paint in temporary copies for SVG 1.1 exporters."""
    tree=ET.parse(source_svg)
    rgba=re.compile(r"rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([0-9.]+)\s*\)")
    for e in tree.iter():
        for attr,opacity in [('fill','fill-opacity'),('stroke','stroke-opacity'),('stop-color','stop-opacity')]:
            match=rgba.fullmatch(e.get(attr,''))
            if match:
                red,green,blue=map(int,match.groups()[:3]);alpha=float(match.group(4))
                if not all(0<=v<=255 for v in [red,green,blue]) or not 0<=alpha<=1:
                    raise ValueError('Invalid SVG paint color')
                e.set(attr,f'#{red:02x}{green:02x}{blue:02x}')
                e.set(opacity,f'{float(e.get(opacity,"1"))*alpha:.6f}')
    ET.register_namespace('','http://www.w3.org/2000/svg')
    tree.write(destination_svg,encoding='utf-8',xml_declaration=True)


def build(output,keep_intermediates=None):
    if not shutil.which('inkscape'):raise SystemExit('Inkscape is required for vector SVG embedding.')
    output=Path(output).resolve();output.parent.mkdir(parents=True,exist_ok=True)
    holder=tempfile.TemporaryDirectory(prefix='forge-ui-pdf-') if keep_intermediates is None else None
    temp=Path(holder.name) if holder else Path(keep_intermediates).resolve();temp.mkdir(parents=True,exist_ok=True)
    base=temp/'ui-report-base.pdf';create_base(base);doc=fitz.open(base)
    for page_index,name in zip(DIAGRAM_PAGES,DIAGRAMS):
        source_svg=ROOT/'docs/architecture/diagrams/mermaid/images'/f'{name}.svg'
        if not source_svg.is_file():raise SystemExit(f'Missing canonical SVG: {source_svg}')
        svg=temp/f'{name}.svg';pdf=temp/f'{name}.pdf';inkscape_compatible_svg(source_svg,svg)
        env=os.environ.copy();env['HOME']=str(temp)
        subprocess.run(['inkscape',str(svg),'--export-type=pdf',f'--export-filename={pdf}'],
                       check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env)
        with fitz.open(pdf) as visual:
            viewport=(fitz.Rect(M+7,H-465,W-M-7,H-352) if page_index in [2,6]
                      else fitz.Rect(M+7,H-470,W-M-7,H-176))
            doc[page_index].show_pdf_page(viewport,visual,0,keep_proportion=True)
    doc.set_metadata({'title':'Forge IDE UI Architecture Report','author':'Homelab architecture',
        'subject':'Proposed Rust owned UI for configured read only Forge IDE inspection',
        'keywords':'Forge, UI, Rust, WASM, architecture, proposed browser integration',
        'creator':'Homelab UI report builder','creationDate':'D:20261102000000Z','modDate':'D:20261102000000Z'})
    doc.save(output,garbage=4,deflate=True);doc.close()
    if holder:holder.cleanup()
    print(output)


if __name__=='__main__':
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output',type=Path,default=Path(__file__).with_name('forge-ide-ui.pdf'))
    ap.add_argument('--keep-intermediates',type=Path)
    args=ap.parse_args();build(args.output,args.keep_intermediates)

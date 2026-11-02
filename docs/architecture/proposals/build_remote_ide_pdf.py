#!/usr/bin/env python3
"""Build the remote IDE visual proposal from its canonical Mermaid SVGs.

Requires Python 3.9+, reportlab, PyMuPDF, and Inkscape on PATH.
Run from anywhere: python3 docs/architecture/proposals/build_remote_ide_pdf.py
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
BASE = 'a11d327a3fd1aadec232a47f1c7cc8d086a7ef0f'
GH = 'https://github.com/roachmaster/homelab'
PROPOSAL = GH + '/blob/main/docs/architecture/proposals/remote-ide.md'
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

NAV = [('Context', 'context'), ('Components', 'components'), ('Contracts', 'contracts'),
       ('Request', 'request'), ('Bootstrap', 'bootstrap'), ('Plan', 'plan')]
DIAGRAMS = ['m13-remote-ide-context', 'm14-remote-ide-components', 'm15-remote-ide-contracts',
            'm16-remote-ide-request', 'm17-remote-ide-bootstrap']


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
    label(c, 'HOMELAB  /  FORGE ARCHITECTURE', M, 580, 8.3, MUTED, True)
    c.setFont(FONT,8.3); c.setFillColor(MUTED)
    c.drawRightString(W-M,580,'PROPOSAL  /  02 NOV 2026')
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
    c.drawRightString(W-M,36,f'{number} / 6')
    link(c,'GitHub',GH,M,15,7.2)
    link(c,'Proposal',PROPOSAL,M+45,15,7.2)
    link(c,'Diagram gallery',GALLERY,M+96,15,7.2)
    link(c,'Front door skill',SKILL,M+178,15,7.2)
    c.setFillColor(MUTED);c.setFont(FONT,7.1)
    c.drawRightString(W-M,15,'Verified baseline '+BASE[:7]+'  |  Proposed contracts are not shipped APIs')


def note(c,title,text,x,top,width=340,color=TEAL):
    label(c,title,x,top,9.4,color,True)
    para(c,text,x,top-10,width,9.1,12.5)


def evidence(c,items,y=72):
    x=M
    for title,url in items:
        width=link(c,title,url,x,y,7.7)
        x+=width+18


def diagram_box(c, index):
    url = GH + '/blob/main/docs/architecture/diagrams/mermaid/images/' + DIAGRAMS[index] + '.svg'
    link(c, 'Open full size SVG', url, W-M-78, 480, 7.1)
    c.setFillColor(BG)
    c.roundRect(M,171,W-2*M,304,8,stroke=0,fill=1)


def create_base(path):
    c=canvas.Canvas(str(path),pagesize=(W,H),pageCompression=1,invariant=1)
    c.setTitle('Remote Forge IDE proposal')
    c.setAuthor('Homelab architecture')
    c.setCreator('Homelab visual proposal builder')
    c.setSubject('Visual proposal for a configured node agent and read only remote project inspection')
    c.setKeywords('Forge, homelab, architecture, UML, remote IDE, proposed node agent')

    base_page(c,1,'context','Remote Forge IDE proposal',
        'Next milestone: run a configured node agent and reuse local project inspection remotely. '
        'Keep one front door and typed contracts. This is a proposal, not a deployed remote IDE.')
    diagram_box(c, 0)
    note(c,'What exists today',
         'A local IDE inspector and an authenticated loopback <b>node.info</b> proof. '
         'The current terminal, jobs and project inspection remain local.',M,150)
    note(c,'What this milestone adds',
         'Named nodes, an agent lifecycle and an allowlisted project alias. '
         'The agent returns a typed report; the caller renders YAML.',410,150)
    evidence(c,[('Current HTTPS boundary',source('src/rust/forge-remote-https/README.md')),
                ('Current IDE scope',source('src/rust/forge-ide/README.md'))])
    c.showPage()

    base_page(c,2,'components','Keep capabilities behind one front door',
        'Thin adapters project input into a canonical envelope. The proposed agent app owns client and server '
        'composition; capabilities do not import HTTP, certificates or the agent application.')
    diagram_box(c, 1)
    label(c,'Proposed package placement',M+15,337,9.4,INK,True)
    package_cards = [
        ('forge-agent','NEW APPLICATION',
         'Client and server composition<br/>Local CLI bridge binding<br/>config / catalog / policy<br/>dispatch / lifecycle', AMBER, AMBER_PALE),
        ('forge-remote-https','PROPOSED ADAPTER EXTENSION',
         'Private IDE codec and typed gateway seam<br/>Implements the capability port<br/>Never imports forge-agent', BLUE, BLUE_PALE),
        ('forge-ide','EXISTING CAPABILITY',
         'task / filesystem / syntax / model<br/>Proposed optional remote contract<br/>Existing CLI remains local', TEAL, TEAL_PALE),
    ]
    for i,(name,status,body,color,fill) in enumerate(package_cards):
        x=M+15+i*232
        c.setFillColor(fill);c.roundRect(x,218,222,104,6,stroke=0,fill=1)
        label(c,status,x+10,304,6.8,color,True)
        label(c,name,x+10,285,11,color,True)
        para(c,body,x+10,272,202,8.7,12.6)
    para(c,'<b>Cargo direction</b>  Application imports adapters and capability contracts. '
           'The capability never imports the application or transport.',M+15,201,W-2*M-30,8.6,12)
    note(c,'Endpoint is not task routing',
         'A configured node name selects the network endpoint and expected identity. '
         'Canonical <b>Header</b> destination and version select a registered operation.',M,150)
    note(c,'Retain the existing capability',
         'Reuse <b>forge-ide</b> model, task, filesystem and syntax modules for trusted projects. '
         'Reports are not secret-redacted; transport and YAML stay at the edges.',410,150)
    evidence(c,[('Remote typed conversation',source('src/rust/forge-remote/src/lib.rs')),
                ('IDE module boundaries',source('src/rust/forge-ide/README.md'))])
    c.showPage()

    base_page(c,3,'contracts','Keep contracts independent of transport',
        'UML style names summarize responsibilities rather than reproduce Rust signatures. '
        'The generic execution and I/O contracts already exist; remote IDE contracts are proposed.')
    diagram_box(c, 2)
    note(c,'Actual contracts',
         '<b>Task</b> owns Input, Output and Error; <b>IoClient</b> owns Request, Response and Error. '
         'Factories construct; providers control acquisition and leases.',M,150)
    note(c,'Proposed composition boundary',
         'A proposed remote inspection client carries a typed request and report. '
         'The current <b>Destination</b> is node-only; it is not already an IDE dispatcher.',410,150)
    evidence(c,[('Task contract',source('src/rust/forge-task/src/lib.rs')),
                ('I/O client contract',source('src/rust/forge-io-client/src/lib.rs')),
                ('Current UML gallery',source('docs/architecture/diagrams/mermaid/m01-contracts.mmd'))])
    c.showPage()

    base_page(c,4,'request','Admit identity before inspecting a project',
        'Keep POST /forge/v1/dispatch and header selected routing. Verified TLS identity is not an HTTP claim, '
        'and authentication never replaces operation and project authorization.')
    diagram_box(c, 3)
    note(c,'Reject before capability work',
         'Check target, version, typed payload, expiry and policy. '
         'Resolve a permitted project alias to a configured root; do not accept an arbitrary remote path.',M,150)
    note(c,'Validate the return path',
         'Bound the report and check responder and correlation metadata. A timeout or lost reply '
         'can follow execution; do not silently retry or claim cancellation.',410,150)
    evidence(c,[('Header and TLS rules',source('src/rust/forge-remote-https/README.md')),
                ('Authorization and reply checks',source('src/rust/forge-remote/src/lib.rs'))])
    c.showPage()

    base_page(c,5,'bootstrap','Compose once before serving requests',
        'Preserve the typed registrar and factory phases. The proposed agent owns explicit startup, '
        'readiness, request serving and shutdown instead of turning a loopback example into a daemon.')
    diagram_box(c, 4)
    note(c,'Configuration before effects',
         'Stage one proposed batch group for runtime project aliases. Reject conflicts, freeze, then invoke '
         'factories. Inject providers and tasks; publish readiness only after successful assembly.',M,150)
    note(c,'Production setup is deliberate',
         'Host certificates, peer mappings, project roots and access policy need explicit setup. '
         'No automatic trust store, firewall or fleet provisioning is part of this proposal.',410,150)
    evidence(c,[('Existing bootstrap sequence',source('docs/architecture/diagrams/mermaid/m02-bootstrap.mmd')),
                ('Current adapter construction',source('src/rust/forge-remote-https/README.md'))])
    c.showPage()

    base_page(c,6,'plan','Ship a read only milestone first',
        'Finish a small remote observation path before adding jobs or mutations. '
        'Every stage needs evidence of its failure behavior as well as its successful result.')

    # Roadmap band. The first two stages define the proposed next milestone.
    stages=[('01','Agent lifecycle','Config, readiness and shutdown',TEAL,TEAL_PALE),
            ('02','Remote inspection','Named node and permitted project',TEAL,TEAL_PALE),
            ('03','Bounded remote jobs','Status, ownership and cancellation',BLUE,BLUE_PALE),
            ('04','Mutations later','Explicit contracts and authorization',AMBER,AMBER_PALE)]
    gap=12; width=(W-2*M-gap*3)/4
    for i,(num,title,detail,color,fill) in enumerate(stages):
        x=M+i*(width+gap)
        c.setFillColor(fill);c.roundRect(x,376,width,97,7,stroke=0,fill=1)
        label(c,num,x+12,451,13,color,True)
        para(c,title,x+12,439,width-24,10.7,13.2,color,True)
        para(c,detail,x+12,407,width-24,8.5,11.2,color)
    chip(c,'NEXT MILESTONE',M,357,TEAL_PALE,TEAL)
    chip(c,'LATER NOT INCLUDED',M+369,357,AMBER_PALE,AMBER)

    # Acceptance matrix with deliberate scope and evidence, not fake pass marks.
    label(c,'Acceptance evidence required before calling it done',M,325,12.5,INK,True)
    rows=[
      ('Identity and access','Wrong peer, wrong target and denied project fail before inspection.'),
      ('Network and protocol','Unreachable node, malformed input, unknown route and incompatible version fail clearly.'),
      ('Bounded work and output','Entry, file, parser and response limits remain explicit; oversized output is rejected.'),
      ('Timeout and lifecycle','Prove start and shutdown behavior; report execution uncertainty after a lost reply.'),
      ('Typed result and features','Preserve report completeness; render YAML only at the edge; omit unselected transport dependencies.')]
    y=303
    for i,(left,right) in enumerate(rows):
        c.setFillColor(BG if i%2==0 else white);c.rect(M,y-36,W-2*M,36,stroke=0,fill=1)
        para(c,left,M+10,y-9,161,9.1,11.8,INK,True)
        para(c,right,M+184,y-8,524,9.2,12.3)
        y-=36
    para(c,'<b>Not in this milestone</b>  Remote shell or arbitrary paths, a full editor or compiler service, '
           'automatic device provisioning, complete GET PUT PATCH DELETE semantics, and Bitwave. '
           'Bitwave remains an optional future codec or carrier, not a prerequisite.',M,109,W-2*M,8.9,12.1,MUTED)
    evidence(c,[('Read the full proposal',PROPOSAL),('Open the design skill',SKILL)],y=64)
    c.showPage();c.save()


def inkscape_compatible_svg(source_svg, destination_svg):
    """Preserve CSS rgba colors for Inkscape versions using SVG 1.1 parsing.

    This conversion touches only a temporary copy, not the canonical asset.
    Alpha is multiplied into the corresponding existing paint opacity.
    """
    tree = ET.parse(source_svg)
    rgba = re.compile(r"rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([0-9.]+)\s*\)")
    for element in tree.iter():
        for attribute, opacity in [('fill', 'fill-opacity'), ('stroke', 'stroke-opacity'),
                                   ('stop-color', 'stop-opacity')]:
            value = element.get(attribute, '')
            match = rgba.fullmatch(value)
            if match:
                red, green, blue = map(int, match.groups()[:3])
                alpha = float(match.group(4))
                if not all(0 <= channel <= 255 for channel in [red, green, blue]) or not 0 <= alpha <= 1:
                    raise ValueError(f'Invalid rgba color in {source_svg}: {value}')
                element.set(attribute, f'#{red:02x}{green:02x}{blue:02x}')
                element.set(opacity, f'{float(element.get(opacity, "1")) * alpha:.6f}')
    ET.register_namespace('', 'http://www.w3.org/2000/svg')
    tree.write(destination_svg, encoding='utf-8', xml_declaration=True)


def build(output, keep_intermediates=None):
    if not shutil.which('inkscape'):
        raise SystemExit('Inkscape is required to preserve canonical SVGs as PDF vectors.')
    output=Path(output).resolve();output.parent.mkdir(parents=True,exist_ok=True)
    holder = tempfile.TemporaryDirectory(prefix='forge-remote-pdf-') if keep_intermediates is None else None
    temp=Path(holder.name) if holder else Path(keep_intermediates).resolve()
    temp.mkdir(parents=True,exist_ok=True)
    base=temp/'proposal-base.pdf';create_base(base)
    doc=fitz.open(base)
    for i,name in enumerate(DIAGRAMS):
        svg=ROOT/'docs/architecture/diagrams/mermaid/images'/f'{name}.svg'
        if not svg.is_file(): raise SystemExit(f'Missing canonical diagram: {svg}')
        pdf=temp/f'{name}.pdf'
        compatible=temp/f'{name}.svg'
        inkscape_compatible_svg(svg,compatible)
        env=os.environ.copy();env['HOME']=str(temp)
        subprocess.run(['inkscape',str(compatible),'--export-type=pdf',f'--export-filename={pdf}'],
                       check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env)
        with fitz.open(pdf) as visual:
            # ReportLab is bottom-up; PyMuPDF is top-down.
            viewport = (fitz.Rect(M+7,H-465,W-M-7,H-355) if i == 1
                        else fitz.Rect(M+7,H-470,W-M-7,H-176))
            doc[i].show_pdf_page(viewport,visual,0,keep_proportion=True,overlay=True)
    doc.set_metadata({'title':'Remote Forge IDE proposal','author':'Homelab architecture',
                      'subject':'Read only node agent and remote project inspection architecture',
                      'keywords':'Forge, homelab, UML, architecture, proposed remote IDE',
                      'creator':'Homelab visual proposal builder',
                      'creationDate':'D:20261102000000Z','modDate':'D:20261102000000Z'})
    doc.save(output,garbage=4,deflate=True);doc.close()
    if holder: holder.cleanup()
    print(output)


if __name__=='__main__':
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output',type=Path,default=Path(__file__).with_name('remote-ide-proposal.pdf'))
    ap.add_argument('--keep-intermediates',type=Path)
    args=ap.parse_args();build(args.output,args.keep_intermediates)

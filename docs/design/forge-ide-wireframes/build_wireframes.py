#!/usr/bin/env python3
"""Build static, editable design SVGs and PNG previews, NOT application assets.

Requires Python 3, Pillow (text bounds), DejaVu fonts and Inkscape. No network,
application source generation, runtime UI, or executable content is produced.
Use --output-dir DIR to render a comparison set without modifying repository files.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
import tempfile
import subprocess
from html import escape
from pathlib import Path
from PIL import ImageFont

C = dict(ink='#202833', muted='#566372', line='#cbd2da', bg='#f4f6f8',
         white='#ffffff', chrome='#e9edf1', selected='#dce8f6', blue='#295d92',
         warning='#80591e', warnbg='#fff3d9', green='#316557', disabled='#7b8692')
FONTS = Path('/usr/share/fonts/truetype/dejavu')

class Sheet:
    def __init__(self, width, height, title, description):
        self.w, self.h = width, height
        self.p = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">',
                  f'<title id="title">{escape(title)}</title>', f'<desc id="desc">{escape(description)}</desc>']
        self.text_count = 0
        self.rect(0, 0, width, height, C['white'])
    def add(self, s): self.p.append(s)
    def group(self, name): self.add(f'<g id="{name}">')
    def end(self): self.add('</g>')
    def rect(self,x,y,w,h,fill='none',stroke=None,r=0,dash=None):
        self.add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}"'+(f' stroke="{stroke}"' if stroke else '')+(f' stroke-dasharray="{dash}"' if dash else '')+'/>')
    def line(self,x,y,x2,y2,color=None,width=1,dash=None):
        self.add(f'<path d="M{x} {y} L{x2} {y2}" fill="none" stroke="{color or C["line"]}" stroke-width="{width}"'+(f' stroke-dasharray="{dash}"' if dash else '')+'/>')
    def text(self,x,y,s,size=14,color=None,bold=False,mono=False,anchor='start',maxw=None):
        s=str(s)
        fontname=('DejaVuSansMono' if mono else 'DejaVuSans')+('-Bold' if bold else '')+'.ttf'
        f=ImageFont.truetype(str(FONTS/fontname),size)
        width=f.getlength(s)
        start=x-width if anchor=='end' else x-width/2 if anchor=='middle' else x
        if maxw is not None and width>maxw+0.5:
            raise ValueError(f'Text too wide: {s!r}: {width:.1f}>{maxw}')
        if start < 0 or start+width > self.w+0.5 or y-size < 0 or y+4 > self.h:
            raise ValueError(f'Text outside sheet: {s!r}')
        self.text_count+=1
        self.add(f'<text x="{x}" y="{y}" font-family="DejaVu Sans{" Mono" if mono else ""}, sans-serif" font-size="{size}" fill="{color or C["ink"]}" font-weight="{700 if bold else 400}" text-anchor="{anchor}">{escape(s)}</text>')
    def multiline(self,x,y,lines,size=14,color=None,bold=False,step=21,maxw=None):
        for i,s in enumerate(lines): self.text(x,y+i*step,s,size,color,bold,maxw=maxw)
    def button(self,x,y,w,label,primary=False,disabled=False,focus=False):
        self.rect(x,y,w,30,C['blue'] if primary else C['white'],C['line'],3)
        self.text(x+w/2,y+20,label,14,C['white'] if primary else C['disabled'] if disabled else C['ink'],bold=primary,anchor='middle',maxw=w-12)
        if focus:self.rect(x-3,y-3,w+6,36,'none',C['blue'],4,'3 2')
    def selector(self,x,y,w,label):
        self.rect(x,y,w,30,C['white'],C['line'],3)
        self.text(x+10,y+20,label,14,maxw=w-32)
        self.chevron(x+w-20,y+12)
    def chevron(self,x,y,down=True):
        self.add(f'<path d="M{x} {y} l{4 if down else 0} {4 if down else 8} l{4 if down else 5} {-4 if down else -4}" stroke="{C["muted"]}" stroke-width="1.5" fill="none"/>')
    def fileicon(self,x,y,folder=False):
        if folder:self.add(f'<path d="M{x} {y+3} h5 l2 2 h8 v10 h-15 z" fill="none" stroke="{C["muted"]}" stroke-width="1.2"/>')
        else:self.add(f'<path d="M{x+2} {y+1} h8 l4 4 v12 h-12 z M{x+10} {y+1} v4 h4" fill="none" stroke="{C["muted"]}" stroke-width="1.2"/>')
    def num(self,x,y,n):
        self.add(f'<circle cx="{x}" cy="{y}" r="11" fill="{C["white"]}" stroke="{C["blue"]}" stroke-width="1.5"/>')
        self.text(x,y+4,str(n),12,C['blue'],True,anchor='middle')
    def footer(self, y, title, lines, x=32, maxw=420):
        self.text(x,y,title,14,bold=True,maxw=maxw)
        self.multiline(x,y+24,lines,14,C['muted'],maxw=maxw)
    def finish(self):return '\n'.join(self.p+['</svg>'])+'\n'


def heading(s,letter,title,subtitle):
    s.text(32,40,f'{letter} / {title}',24,bold=True)
    s.text(32,70,subtitle,15,C['muted'])
    s.text(s.w-32,40,'DESIGN ONLY',14,C['blue'],True,anchor='end')
    s.text(s.w-32,70,'Fictional sample data',14,C['muted'],anchor='end')


def toolbar(s,x,y,w,narrow=False):
    s.group('workbench-toolbar')
    s.rect(x,y,w,48,C['chrome'],C['line'])
    s.text(x+16,y+30,'FORGE IDE',14,bold=True)
    if narrow:
        s.selector(x+130,y+9,170,'Node: lab-example')
        s.selector(x+308,y+9,240,'Project: sample-workspace')
        s.button(x+556,y+9,98,'Inspect',primary=True)
        s.button(x+662,y+9,162,'View options †')
    else:
        s.selector(x+136,y+9,178,'Node: lab-example')
        s.selector(x+322,y+9,246,'Project: sample-workspace')
        s.button(x+576,y+9,98,'Inspect',primary=True)
        s.button(x+682,y+9,150,'Cancel local wait',disabled=True)
        s.button(x+850,y+9,166,'View options †')
        s.text(x+w-16,y+30,'Read-only structural inspection',14,C['muted'],anchor='end')
    s.end()


def pane_header(s,x,y,w,label,marker=False):
    s.rect(x,y,w,34,C['bg'])
    s.line(x,y+34,x+w,y+34)
    s.text(x+14,y+23,label,14,bold=True)
    s.text(x+w-14,y+23,'− †',14,C['muted'],anchor='end')


def splitter(s,x,y,h):
    s.rect(x,y,6,h,C['chrome'])
    for i in range(3):s.line(x+2,y+h/2-6+i*5,x+4,y+h/2-6+i*5,C['muted'])


def explorer(s,x,y,w,h,partial=False):
    s.group('explorer')
    s.rect(x,y,w,h,C['bg'])
    pane_header(s,x,y,w,'Explorer')
    s.rect(x+12,y+46,w-24,30,C['white'],C['line'],3)
    s.text(x+23,y+66,'Filter entry names… †',14,C['muted'])
    rows=[('sample-workspace',0,True,False),('Cargo.toml',1,False,False),('README.md',1,False,False),
          ('src',1,True,False),('lib.rs',2,False,not partial),('model.rs',2,False,False),
          ('remote_project_reg…',2,False,False),('transport',2,True,False),('mod.rs',3,False,False),
          ('client.rs',3,False,partial),('tests',1,True,False),('inspection.rs',2,False,False)]
    ry=y+88
    for i,(name,depth,folder,selected) in enumerate(rows):
        yy=ry+i*28
        if selected:s.rect(x+4,yy,w-8,28,C['selected'])
        xx=x+12+depth*16
        if folder:s.chevron(xx,yy+11)
        s.fileicon(xx+14,yy+5,folder)
        s.text(xx+37,yy+20,name,14,bold=folder,maxw=w-(xx-x)-41)
        if partial and name in ('client.rs','inspection.rs'):s.text(x+w-16,yy+20,'!',14,C['warning'],True,anchor='end')
        if not partial and name=='model.rs':s.rect(x+6,yy+2,w-12,24,'none',C['blue'],2,'3 2')
    s.line(x+45,ry+28,x+45,ry+12*28-6)
    s.line(x+61,ry+4*28,x+61,ry+10*28-6)
    if not partial:
        s.line(x+12,y+h-62,x+w-12,y+h-62)
        s.multiline(x+14,y+h-39,['Focused: model.rs','Selected: src/lib.rs'],13,C['muted'],step=19)
    s.end()

DECL=[('model','module','3:0–3:10',0),('transport','module','4:0–4:14',0),
      ('Workspace','struct','7:0–10:1',0),('root','field','8:4–8:16',1),
      ('Inspect','trait','13:0–15:1',0),('inspect','trait method','14:4–14:22',1),
      ('Inspect for Workspace','impl','18:0–24:1',0),('inspect','impl method','19:4–23:5',1),
      ('open_workspace','function','27:0–31:1',0)]


def desktop_ready():
    s=Sheet(1440,1080,'Forge IDE workbench wireframe — ready',
        'Design-only proposed desktop workbench. Fictional structural report with Explorer, central file overview, optional Outline, Problems and status. No source editor. Numbered notes distinguish existing data from proposed interaction.')
    heading(s,'A','A workbench, not a dashboard','Desktop · complete report · neutral wireframe, not a final theme')
    x,y,w=32,112,1376
    toolbar(s,x,y,w)
    s.rect(x,160,w,32,C['white'],C['line'])
    s.text(48,182,'lab-example / sample-workspace / src / lib.rs',14,C['muted'])
    s.text(1392,182,'Complete report  •  8 files',14,C['green'],anchor='end')
    by,bh=192,542
    explorer(s,x,by,270,bh)
    splitter(s,302,by,bh)
    cx,cw=308,846
    s.group('file-overview')
    pane_header(s,cx,by,cw,'File overview   /   lib.rs')
    s.text(cx+22,260,'lib.rs',22,bold=True)
    s.text(cx+22,287,'File  ·  32 lines  ·  684 bytes  ·  Content: text',14,C['muted'])
    s.text(cx+cw-22,260,'READ ONLY',12,C['muted'],True,anchor='end')
    s.line(cx+22,307,cx+cw-22,307)
    s.text(cx+22,334,'Declaration',13,C['muted'],True)
    s.text(cx+418,334,'Kind',13,C['muted'],True)
    s.text(cx+642,334,'Span',13,C['muted'],True)
    for i,(name,kind,span,depth) in enumerate(DECL):
        yy=348+i*28
        if i==6:s.rect(cx+10,yy,cw-20,28,C['selected'])
        s.text(cx+22+depth*18,yy+19,name,14,mono=True)
        s.text(cx+418,yy+19,kind,14,C['muted'])
        s.text(cx+642,yy+19,span,13,C['muted'],mono=True)
        s.line(cx+22,yy+28,cx+cw-22,yy+28)
    s.text(cx+22,625,'Selected declaration',13,C['muted'],True)
    s.text(cx+22,651,'Inspect for Workspace',17,bold=True)
    s.text(cx+22,679,'Impl target: Workspace    Trait: Inspect',14)
    s.text(cx+22,701,'Unsafe: false   ·   Negative: false',14,C['muted'])
    s.text(cx+cw-22,722,'Spans: 1-based lines; 0-based columns; end exclusive',12,C['muted'],anchor='end')
    s.end()
    splitter(s,1154,by,bh)
    s.group('outline')
    ox,ow=1160,248
    s.rect(ox,by,ow,bh,C['bg'])
    pane_header(s,ox,by,ow,'Outline')
    s.text(ox+14,254,'lib.rs  /  9 declarations',13,C['muted'])
    for i,(name,kind,span,depth) in enumerate(DECL):
        yy=270+i*30
        if i==6:s.rect(ox+4,yy,ow-8,30,C['selected'])
        label=name if len(name)<23 else name[:21]+'…'
        glyph={'module':'M','struct':'S','field':'·','trait':'T','trait method':'m','impl':'I','impl method':'m','function':'f'}[kind]
        s.rect(ox+12+depth*14,yy+6,17,18,C['white'],C['line'],2)
        s.text(ox+20.5+depth*14,yy+19,glyph,11,C['muted'],True,anchor='middle')
        s.text(ox+36+depth*14,yy+20,label,14,maxw=ow-46-depth*14)
    s.line(ox+14,562,ox+ow-14,562)
    s.multiline(ox+14,587,['Quick navigation; details','stay in the main workspace.','No source text is requested.'],13,C['muted'],step=21)
    s.end()
    s.group('problems')
    s.rect(x,734,w,83,C['white'],C['line'])
    s.rect(x,734,w,34,C['bg'])
    s.line(700,737,740,737,C['muted'],2)
    s.text(48,757,'Problems  0   †',14,bold=True)
    s.text(1392,757,'Collapse †  −',14,C['muted'],anchor='end')
    s.text(52,795,'No inspection diagnostics in this report. This is not a compiler or test result.',14,C['muted'])
    s.end()
    s.rect(x,817,w,30,C['chrome'],C['line'])
    s.text(48,838,'Local host session  •  Complete report',13,C['green'])
    s.text(1392,838,'lab-example / sample-workspace  ·  No file editing or execution',13,C['muted'],anchor='end')
    for px,py,n in [(157,137,1),(151,210,2),(580,210,3),(1261,210,4),(218,751,5)]:s.num(px,py,n)
    s.footer(887,'1  Compact context + one primary action',['Node/project choices remain configured.','View options † only manages local panes.'],32,422)
    s.footer(887,'2  Real hierarchy; visible keyboard focus',['Selection uses fill; focus uses a dashed ring.','Long names reveal fully on focus or hover †.'],504,430)
    s.footer(887,'3–4  A dominant, read-only workspace',['File/declaration data exists; this layout is new.','Resizable / collapsible panes † need generator work.'],976,432)
    s.line(32,964,1408,964)
    s.text(32,991,'EXISTING CAPABILITY',12,C['green'],True)
    s.text(209,991,'Inspect, tree selection, Rust declarations, spans, partial results, local cancel/retry.',14)
    s.text(32,1020,'PROPOSED INTERACTION †',12,C['blue'],True)
    s.text(235,1020,'Workbench layout, filter, view controls, pane resizing/collapse, Problems index.',14)
    s.text(32,1050,'5  Problems summarizes inspection diagnostics only. Fictional paths, symbols and counts; not an implementation screenshot.',13,C['muted'])
    return s


def desktop_partial():
    s=Sheet(1440,1080,'Forge IDE workbench wireframe — partial report',
        'Design-only desktop partial inspection. A file with unavailable Rust structure is selected. Existing diagnostic strings appear in a proposed Problems index without invented compiler severity or precise source location. Other accepted report entries remain browsable.')
    heading(s,'B','Partial is a usable result, not success','Desktop · diagnostics visible · no compiler-error or source-editor claims')
    x,y,w=32,112,1376
    toolbar(s,x,y,w)
    s.rect(x,160,w,40,C['warnbg'],C['line'])
    s.text(48,186,'!  Partial inspection — some files or Rust declarations could not be inspected.',14,C['warning'],True)
    s.text(1392,186,'Review Problems below',14,C['warning'],anchor='end')
    by,bh=200,430
    explorer(s,x,by,270,bh,True)
    splitter(s,302,by,bh)
    cx,cw=308,846
    s.group('partial-overview')
    pane_header(s,cx,by,cw,'File overview   /   client.rs')
    s.text(cx+22,268,'src / transport / client.rs',21,bold=True)
    s.text(cx+22,297,'File  ·  86 lines  ·  2,640 bytes  ·  Content: rust_unavailable',14,C['muted'])
    s.line(cx+22,320,cx+cw-22,320)
    s.text(cx+22,357,'Rust structure unavailable for this file',20,bold=True)
    s.multiline(cx+22,391,['The file was counted, but its Rust declarations could not be inspected.',
                                'No declaration list is shown; an empty list is not proof of no symbols.'],14,C['muted'],maxw=cw-44)
    s.text(cx+22,459,'Inspection diagnostic',14,bold=True)
    s.rect(cx+22,476,cw-44,54,C['warnbg'])
    s.text(cx+36,508,'Rust syntax could not be parsed.',14,C['warning'])
    s.text(cx+22,562,'Other files in this accepted report remain available in Explorer.',14,C['muted'])
    s.text(cx+22,597,'Read-only structural inspection  ·  No source body or editing API',13,C['muted'])
    s.end()
    splitter(s,1154,by,bh)
    s.rect(1160,by,248,bh,C['bg'])
    pane_header(s,1160,by,248,'Outline')
    s.multiline(1174,277,['No Rust outline available','for client.rs in this report.'],14,C['muted'],maxw=220)
    s.multiline(1174,347,['Select another Rust file','or inspect again after the','underlying issue is resolved.'],13,C['muted'],maxw=220)
    s.rect(x,630,w,187,C['white'],C['line'])
    s.rect(x,630,w,34,C['bg'])
    s.line(700,633,740,633,C['muted'],2)
    s.text(48,653,'Problems  2   †',14,bold=True)
    s.text(1392,653,'Inspection diagnostics only    − †',14,C['muted'],anchor='end')
    s.text(50,690,'File',13,C['muted'],True)
    s.text(445,690,'Diagnostic text',13,C['muted'],True)
    s.text(1270,690,'Origin',13,C['muted'],True)
    s.rect(x+4,703,w-8,34,C['selected'])
    s.text(50,726,'src/transport/client.rs',14,mono=True)
    s.text(445,726,'Rust syntax could not be parsed.',14)
    s.text(1270,726,'Inspection',14,C['muted'])
    s.line(48,737,1392,737)
    s.text(50,763,'tests/inspection.rs',14,mono=True)
    s.text(445,763,'File could not be read.',14)
    s.text(1270,763,'Inspection',14,C['muted'])
    s.text(50,800,'Proposed row activation selects the associated file; it does not jump into an editor.',13,C['muted'])
    s.rect(x,817,w,30,C['chrome'],C['line'])
    s.text(48,838,'Local host session  •  Partial report  •  2 diagnostics',13,C['warning'])
    s.text(1392,838,'Not a build / test / type-check result',13,C['muted'],anchor='end')
    s.footer(887,'Keep accepted structure available',['Incomplete reports remain explicitly partial.','Do not replace missing data with fake zeroes.'],32,422)
    s.footer(887,'Explain the selected file in place',['Existing content status + diagnostic strings.','No invented severity or precise error location.'],504,430)
    s.footer(887,'Problems is a proposed local index †',['Aggregate diagnostics from the accepted report.','Selection changes local view; no new remote call.'],976,432)
    s.line(32,964,1408,964)
    s.text(32,993,'EXISTING',12,C['green'],True)
    s.text(123,993,'Partial reports, content status and per-entry diagnostics. Example text and counts are fictional.',14)
    s.text(32,1023,'PROPOSED †',12,C['blue'],True)
    s.text(145,1023,'Workbench layout, aggregate Problems navigation, filters, view options and pane controls.',14)
    s.text(32,1051,'A retry is a new inspection, not a repair action. Missing declarations are not compiler diagnostics.',13,C['muted'])
    return s


def narrow_unavailable():
    s=Sheet(1000,1120,'Forge IDE workbench wireframe — narrow unavailable state',
        'Design-only compact desktop width with side panes collapsed. A failed inspection has no accepted report; recovery uses configured selection and a new Inspect request. Proposed visible pane tabs restore views. State notes show initial, loading and cancelled behavior without claiming remote cancellation.')
    heading(s,'C','Preserve context at narrow widths','Compact desktop · unavailable state · do not squeeze three columns')
    x,y,w=32,112,936
    toolbar(s,x,y,w,True)
    s.rect(x,160,w,36,C['white'],C['line'])
    s.text(48,184,'lab-example / sample-workspace',14,C['muted'])
    s.text(952,184,'Read-only inspection',14,C['muted'],anchor='end')
    s.rect(x,196,w,40,C['bg'],C['line'])
    s.button(44,201,132,'Explorer †')
    s.button(184,201,144,'File overview',primary=True)
    s.button(336,201,130,'Outline †')
    s.text(952,222,'Side panes collapsed †',13,C['muted'],anchor='end')
    s.rect(x,236,w,370,C['white'],C['line'])
    s.text(76,292,'Inspection unavailable',24,bold=True)
    s.multiline(76,328,['No report was accepted for this request.',
                        'Check the configured node, then try again. The UI cannot confirm',
                        'remote completion if a response is missing.'],15,C['muted'],step=24,maxw=820)
    s.button(76,422,154,'Retry inspection',primary=True,focus=True)
    s.text(250,442,'New request; no automatic retry loop.',14,C['muted'])
    s.line(76,481,924,481)
    s.text(76,514,'No stale tree, outline or selected-file details are shown.',15)
    s.text(76,547,'The current node/project selection stays visible above.',14,C['muted'])
    s.text(76,579,'No source file access, editor, terminal, Run or Debug controls.',13,C['muted'])
    s.rect(x,606,w,90,C['bg'],C['line'])
    s.text(48,633,'Problems †',14,bold=True)
    s.text(48,668,'No inspection report available. Request failure is shown above, not as a code problem.',14,C['muted'])
    s.rect(x,696,w,30,C['chrome'],C['line'])
    s.text(48,717,'Unavailable  •  No current report',13,C['warning'])
    s.text(952,717,'Local host session',13,C['muted'],anchor='end')
    s.text(32,770,'State copy for the same workspace',17,bold=True)
    states=[('BEFORE FIRST INSPECT','Choose an allowed node and project, then Inspect. No report yet.'),
            ('WHILE LOADING','Inspect disabled; Cancel local wait enabled. No old report presented as current.'),
            ('AFTER LOCAL CANCEL','Stopped locally. Remote work may still finish; retry creates a new request.')]
    for i,(label,detail) in enumerate(states):
        yy=802+i*64
        s.line(32,yy-8,968,yy-8)
        s.text(32,yy+14,label,12,C['muted'],True)
        s.text(32,yy+39,detail,14,maxw=936)
    s.line(32,997,968,997)
    s.text(32,1026,'EXISTING',12,C['green'],True)
    s.text(123,1026,'Failure, loading, local cancel and retry semantics; no stale report.',14)
    s.text(32,1056,'PROPOSED †',12,C['blue'],True)
    s.text(145,1056,'Single-pane switching; focus returns to its visible trigger on close.',14)
    s.text(32,1087,'Design review only. No browser behavior or accessibility conformance was tested by these images.',13,C['muted'])
    return s


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir',type=Path,default=Path(__file__).resolve().parent)
    args=parser.parse_args()
    args.output_dir.mkdir(parents=True,exist_ok=True)
    report=[]
    for stem,make in [('forge-ide-workbench-desktop',desktop_ready),('forge-ide-workbench-partial',desktop_partial),('forge-ide-workbench-narrow',narrow_unavailable)]:
        sheet=make()
        svg=args.output_dir/(stem+'.svg');png=args.output_dir/(stem+'.png')
        svg.write_text(sheet.finish(),encoding='utf-8')
        with tempfile.TemporaryDirectory(prefix='forge-wireframe-render-') as temp:
            env = dict(os.environ, INKSCAPE_PROFILE_DIR=temp, XDG_CACHE_HOME=temp)
            subprocess.run(['inkscape',str(svg),'--export-type=png','--export-filename='+str(png),'--export-dpi=96'],check=True,capture_output=True,env=env)
        report.append({'name':stem,'width':sheet.w,'height':sheet.h,'text_nodes':sheet.text_count,
                       'svg_sha256':hashlib.sha256(svg.read_bytes()).hexdigest(),
                       'png_sha256':hashlib.sha256(png.read_bytes()).hexdigest()})
    print(json.dumps(report,indent=2))

if __name__=='__main__':main()

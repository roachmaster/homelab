//! Audited reusable browser-control and transport emitters. These primitives
//! never know IDE node names, report algorithms or application state transitions.
//! Typed application declarations configure controls and HTTP field projection.
pub(crate) const MAIN: &str = r#"'use strict';
// GENERATED reusable DOM/pane primitives. All semantic state is worker WASM-owned.
let worker=null;
let fatalState=false;
let actionByElement=new WeakMap();
let focusKeyByElement=new WeakMap();
let inputDrafts=new WeakMap();
let presentationSequence=0;
const initialText=new Map([...document.querySelectorAll('[data-bind-text]')].map(n=>[n,n.textContent]));
const layouts=new Map();
let typeahead=new WeakMap();
function element(id){return typeof id==='string'?document.getElementById(id):null;}
function visible(n){return !!n && n.isConnected && !n.closest('[hidden]');}
function tell(event,sequence){if(worker && !fatalState)worker.postMessage(sequence===undefined?event:{event,presentation_sequence:sequence});}
function sendAction(n,value,sequence){
  if(!visible(n)||n.disabled)return;
  const bound=actionByElement.get(n);
  const action=bound || (n.hasAttribute('data-dynamic-action')?null:{kind:n.dataset.action});
  if(action && action.kind)tell(value===undefined?action:{...action,value},sequence);
}
function clearProtected(){
  // Synchronous: never leave catalog/report pixels behind on pagehide or bfcache restore.
  for(const n of document.querySelectorAll('[data-protected]'))n.hidden=true;
  for(const n of document.querySelectorAll('.fg-screen:not([data-protected])'))n.hidden=false;
  for(const [n,text] of initialText){n.textContent=text;delete n.dataset.tone;}
  for(const n of document.querySelectorAll('select[data-action]')){n.replaceChildren();n.value='';n.removeAttribute('title');delete n.dataset.options;n.disabled=true;}
  for(const n of document.querySelectorAll('input')){n.value='';n.setCustomValidity('');n.removeAttribute('aria-invalid');if(n.hasAttribute('data-action'))n.disabled=true;}
  for(const n of document.querySelectorAll('button[data-action],form[data-action] button'))n.disabled=true;
  for(const n of document.querySelectorAll('.fg-tree,.fg-details')){n.replaceChildren();n.scrollTop=0;}
  for(const n of document.querySelectorAll('[data-name-for]'))n.textContent='';
  for(const n of document.querySelectorAll('.fg-pane-body'))n.scrollTop=0;
  actionByElement=new WeakMap();focusKeyByElement=new WeakMap();typeahead=new WeakMap();inputDrafts=new WeakMap();presentationSequence=0;
  for(const state of layouts.values()){state.focus.clear();state.context.clear();state.resetScroll.clear();}
}
function stop(){const old=worker;worker=null;if(old)old.terminate();clearProtected();}
function fatal(message){
  if(fatalState)return;fatalState=true;stop();
  document.querySelector('.fg-app').hidden=true;
  element('runtime-message').textContent=typeof message==='string'?message.slice(0,512):'Application stopped. Reload to retry.';
  element('runtime-recovery').hidden=false;
  element('runtime-reload').focus();
}
function focusRow(row){
  if(!row||!visible(row))return;
  const tree=row.closest('[role="tree"]');
  for(const n of tree.querySelectorAll('.fg-row-select'))n.tabIndex=n===row?0:-1;
  row.focus();row.scrollIntoView({block:'nearest'});
}
function apply(view,acknowledgedSequence){
  if(!view||typeof view!=='object')throw new Error('Invalid view');
  const focus=document.activeElement;
  const focusKey=focusKeyByElement.get(focus);
  const priorTree=focus?.closest?.('[role="tree"]');
  for(const b of view.visibility||[]){const n=element(b.id);if(n)n.hidden=!b.visible;}
  for(const b of view.texts||[]){const n=element(b.id);if(n){if(n.textContent!==b.text)n.textContent=b.text;n.dataset.tone=['neutral','accent','warning','error'].includes(b.tone)?b.tone:'neutral';}}
  for(const b of view.buttons||[]){const n=element(b.id);if(n){n.disabled=b.disabled;actionByElement.set(n,b.action);}}
  for(const b of view.inputs||[]){
    const n=element(b.id);if(!n)continue;
    const context=JSON.stringify(b.action);const draft=inputDrafts.get(n);
    const sameContext=draft&&draft.context===context&&!b.disabled;
    // Browser drafts are presentation, never an accepted filter. Only the view
    // acknowledging this edit (or a later edit) can replace its same-context text.
    const pending=sameContext&&(draft.invalid||!Number.isSafeInteger(acknowledgedSequence)||acknowledgedSequence<draft.sequence);
    if(!pending){if(n.value!==b.value)n.value=b.value;inputDrafts.delete(n);n.setCustomValidity('');n.removeAttribute('aria-invalid');}
    n.disabled=b.disabled;actionByElement.set(n,b.action);n.dataset.maxBytes=String(b.max_bytes);
  }
  for(const b of view.choices||[]){
    const n=element(b.id);if(!n)continue;
    const signature=JSON.stringify(b.options);
    if(n.dataset.options!==signature){n.replaceChildren(...b.options.map(o=>{const x=document.createElement('option');x.value=o.value;x.textContent=o.label;return x;}));n.dataset.options=signature;}
    n.value=b.value;n.disabled=b.disabled;n.title=n.selectedOptions[0]?.textContent||'';
  }
  for(const b of view.lists||[]){
    const n=element(b.id);if(!n)continue;
    const prior=new Map([...n.children].filter(x=>x.dataset.rowId).map(x=>[x.dataset.rowId,x]));
    const keep=new Set();let rowIndex=0;
    for(const row of b.rows){
      const key=String(row.id);keep.add(key);let item=prior.get(key);
      if(!item){
        item=document.createElement('div');item.dataset.rowId=key;
        const toggle=document.createElement('button');toggle.type='button';toggle.className='fg-toggle';toggle.tabIndex=-1;
        const select=document.createElement('button');select.type='button';select.className='fg-row-select';select.setAttribute('role','treeitem');select.tabIndex=-1;
        const glyph=document.createElement('span');glyph.className='fg-glyph';glyph.setAttribute('aria-hidden','true');
        const label=document.createElement('span');label.className='fg-row-label';
        const secondary=document.createElement('span');secondary.className='fg-row-secondary';select.append(glyph,label,secondary);item.append(toggle,select);
        toggle.addEventListener('click',()=>sendAction(toggle));select.addEventListener('click',()=>sendAction(select));
      }
      item.className='fg-tree-row fg-depth-'+Math.min(64,Math.max(0,Number(row.depth)||0));item.dataset.selected=String(row.selected);item.dataset.parent=row.parent??'';
      const [toggle,select]=item.children;
      toggle.hidden=row.expanded===null||!row.toggle;toggle.disabled=!row.toggle;toggle.textContent=row.expanded?'▾':'▸';toggle.setAttribute('aria-label',(row.expanded?'Collapse ':'Expand ')+row.label);
      // Includes accepted context, but never persists it in URLs, storage or DOM attributes.
      focusKeyByElement.set(toggle,b.id+':'+key+':toggle:'+JSON.stringify(row.toggle));focusKeyByElement.set(select,b.id+':'+key+':select:'+JSON.stringify(row.action));
      actionByElement.set(toggle,row.toggle);actionByElement.set(select,row.action);
      select.setAttribute('aria-level',String(row.depth+1));select.setAttribute('aria-selected',String(row.selected));
      if(row.expanded===null)select.removeAttribute('aria-expanded');else select.setAttribute('aria-expanded',String(row.expanded));
      select.children[0].textContent=row.glyph||'';select.children[1].textContent=row.label;select.children[2].textContent=row.secondary;
      select.setAttribute('aria-label',row.kind_label?row.label+', '+row.kind_label:row.label);select.title=row.label;
      // Preserve stable rows and their scroll anchor; moving every existing node
      // would transiently reorder the list even when its accepted order is unchanged.
      const at=n.children[rowIndex++];if(at!==item)n.insertBefore(item,at||null);
    }
    for(const x of [...n.children])if(!x.dataset.rowId||!keep.has(x.dataset.rowId))x.remove();
    if(!b.rows.length){const p=document.createElement('p');p.className='fg-empty';p.textContent=b.empty_message;n.append(p);}
    const rows=[...n.querySelectorAll('.fg-row-select')];
    const tab=rows.find(x=>focusKeyByElement.get(x)===focusKey)||rows.find(x=>x.tabIndex===0)||rows.find(x=>x.getAttribute('aria-selected')==='true')||rows[0];
    for(const row of rows)row.tabIndex=row===tab?0:-1;
    n.tabIndex=rows.length?-1:0;
    const readout=element(b.id+'-full-name');if(readout && (!focusKey || !rows.some(x=>focusKeyByElement.get(x)===focusKey)))readout.textContent='';
  }
  for(const b of view.tables||[]){
    const n=element(b.id);if(!n)continue;
    const parts=[];for(const row of b.rows){const property=document.createElement('div');property.className='fg-property';const t=document.createElement('dt');t.textContent=row.label;const d=document.createElement('dd');d.textContent=row.value;property.append(t,d);parts.push(property);}n.replaceChildren(...parts);
  }
  for(const b of view.panes||[]){
    for(const state of layouts.values()){
      const pane=state.panes.find(p=>p.id===b.id);if(!pane)continue;
      const context=JSON.stringify(b.context);
      if(state.context.get(b.id)!==context){
        state.context.set(b.id,context);
        // An external context change invalidates old pane focus/scroll. Direct
        // in-pane navigation keeps its current focus and scrolling ownership.
        if(!pane.contains(focus)){
          state.focus.delete(b.id);
          state.resetScroll.add(b.id);
        }
      }
    }
  }
  for(const state of layouts.values())updateLayout(state);
  if(focusKey){const same=[...document.querySelectorAll('.fg-row-select,.fg-toggle')].find(n=>focusKeyByElement.get(n)===focusKey);if(visible(same)){same.focus({preventScroll:true});return;}}
  // Only repair focus that this rendering invalidated. Background results do not steal it.
  if(focus!==document.body && (!focus.isConnected||!visible(focus))){
    const target=visible(priorTree)?priorTree.querySelector('.fg-row-select[tabindex="0"]')||priorTree:document.querySelector('.fg-screen:not([hidden]) select:not(:disabled),.fg-screen:not([hidden]) input:not(:disabled),.fg-screen:not([hidden]) button:not(:disabled)');
    if(visible(target))target.focus({preventScroll:true});
  }
}
function start(){
  if(worker||fatalState)return;
  clearProtected();
  try{
    const owned=new Worker('/worker.js');worker=owned;
    owned.onmessage=({data})=>{if(worker!==owned||fatalState)return;try{if(data.kind==='view')apply(data.view,data.presentation_sequence);else if(data.kind==='fatal')fatal(data.message);}catch(_){fatal('Could not display the checked view. Reload to retry.');}};
    owned.onerror=()=>{if(worker===owned)fatal('The Rust worker could not start. Reload to retry.');};
  }catch(_){fatal('This browser cannot start the required worker. Reload to retry.');}
}
for(const n of document.querySelectorAll('select[data-action]'))n.addEventListener('change',()=>sendAction(n,n.value));
for(const n of document.querySelectorAll('button[data-action]'))n.addEventListener('click',()=>sendAction(n));
for(const n of document.querySelectorAll('input[data-action]'))n.addEventListener('input',()=>{
  // The declared byte cap is a codec bound, not a second query/filter implementation.
  if(!visible(n)||n.disabled||!actionByElement.get(n))return;
  const context=JSON.stringify(actionByElement.get(n));const max=Number(n.dataset.maxBytes)||256;
  if(new TextEncoder().encode(n.value).length>max){inputDrafts.set(n,{context,invalid:true});n.setCustomValidity('Use at most '+max+' UTF-8 bytes. This draft has not been applied.');n.setAttribute('aria-invalid','true');n.reportValidity();return;}
  if(presentationSequence>=2147483647){fatal('The presentation message limit was reached. Reload to retry.');return;}
  const sequence=++presentationSequence;inputDrafts.set(n,{context,sequence,invalid:false});
  n.setCustomValidity('');n.removeAttribute('aria-invalid');sendAction(n,n.value,sequence);
});
for(const n of document.querySelectorAll('form[data-action]'))n.addEventListener('submit',e=>{e.preventDefault();const input=n.querySelector('input[name="value"]');if(!input||n.querySelector('button')?.disabled)return;const value=input.value;input.value='';sendAction(n,value);});
for(const n of document.querySelectorAll('[data-reload]'))n.addEventListener('click',()=>location.reload());
for(const tree of document.querySelectorAll('[role="tree"]')){
  tree.addEventListener('focusin',e=>{
    const row=e.target.closest('.fg-row-select');if(!row)return;
    for(const n of tree.querySelectorAll('.fg-row-select'))n.tabIndex=n===row?0:-1;
    const readout=element(tree.id+'-full-name');if(readout)readout.textContent=row.querySelector('.fg-row-label').textContent;
  });
  tree.addEventListener('keydown',e=>{
    if(e.altKey||e.ctrlKey||e.metaKey)return;
    const rows=[...tree.querySelectorAll('.fg-row-select')];const index=rows.indexOf(document.activeElement);if(index<0)return;
    const current=rows[index];let next=index;
    if(e.key==='ArrowDown')next=Math.min(index+1,rows.length-1);
    else if(e.key==='ArrowUp')next=Math.max(index-1,0);
    else if(e.key==='Home')next=0;
    else if(e.key==='End')next=rows.length-1;
    else if(e.key==='ArrowRight'){
      if(current.getAttribute('aria-expanded')==='false')sendAction(current.parentElement.children[0]);
      else {const child=rows.find(n=>n.parentElement.dataset.parent===current.parentElement.dataset.rowId);if(child)focusRow(child);}
      e.preventDefault();return;
    }else if(e.key==='ArrowLeft'){
      if(current.getAttribute('aria-expanded')==='true'&&actionByElement.get(current.parentElement.children[0]))sendAction(current.parentElement.children[0]);
      else {const parent=rows.find(n=>n.parentElement.dataset.rowId===current.parentElement.dataset.parent);if(parent)focusRow(parent);}
      e.preventDefault();return;
    }else if(e.key.length===1 && e.key!==' '){
      const now=Date.now();const prior=typeahead.get(tree);const text=((prior&&now-prior.at<700?prior.text:'')+e.key.toLocaleLowerCase()).slice(0,256);typeahead.set(tree,{text,at:now});
      const ordered=rows.slice(index+1).concat(rows.slice(0,index+1));const target=ordered.find(n=>n.querySelector('.fg-row-label').textContent.toLocaleLowerCase().startsWith(text));if(target)focusRow(target);e.preventDefault();return;
    }else return;
    e.preventDefault();focusRow(rows[next]);
  });
}
function paneControl(state,pane){const options=element(state.root.id+'-options');if(options)options.open=true;return element(pane.id+'-toggle')||element(pane.id+'-switch');}
function updateLayout(state){
  state.root.dataset.narrow=String(state.narrow.matches);
  if(!state.narrow.matches && state.grid.clientWidth){
    const sides=state.panes.filter(p=>state.open.has(p.id)&&(p.dataset.pane==='leading'||p.dataset.pane==='trailing'));
    let excess=sides.reduce((sum,p)=>sum+state.sizes.get(p.id)+6,Number(state.main.dataset.min))-state.grid.clientWidth;
    for(const pane of sides.slice().reverse()){if(excess<=0)break;const old=state.sizes.get(pane.id);const next=Math.max(Number(pane.dataset.min),old-excess);state.sizes.set(pane.id,next);excess-=old-next;}
  }
  for(const pane of state.panes){
    const open=state.open.has(pane.id);const show=open&&(!state.narrow.matches||state.active===pane.id);
    pane.hidden=!show;
    // A hidden pane has no scroll box. Apply the queued reset only once visible.
    if(show&&state.resetScroll.has(pane.id)){
      const body=pane.querySelector('.fg-pane-body');if(body)body.scrollTop=0;
      state.resetScroll.delete(pane.id);
    }
    const position=pane.dataset.pane;const size=state.sizes.get(pane.id);
    if(position!=='main'){state.grid.style.setProperty('--'+position+'-size',open?size+'px':'0px');state.grid.style.setProperty('--'+position+'-divider',open?'6px':'0px');}
    const separator=element(pane.id+'-separator');if(separator){separator.hidden=!show||state.narrow.matches;separator.setAttribute('aria-valuenow',String(size));}
    const toggle=element(pane.id+'-toggle');if(toggle)toggle.setAttribute('aria-expanded',String(open));
    const tab=element(pane.id+'-switch');if(tab)tab.setAttribute('aria-pressed',String(state.active===pane.id));
  }
}
function togglePane(state,pane){
  const focused=pane.contains(document.activeElement)||document.activeElement===element(pane.id+'-separator');
  if(state.open.has(pane.id)){state.open.delete(pane.id);if(state.active===pane.id)state.active=state.main.id;}
  else {state.open.add(pane.id);if(state.narrow.matches)state.active=pane.id;}
  updateLayout(state);if(focused)paneControl(state,pane)?.focus();
}
function resizePane(state,pane,value){state.sizes.set(pane.id,Math.max(Number(pane.dataset.min),Math.min(Number(pane.dataset.max),Math.round(value))));updateLayout(state);}
for(const root of document.querySelectorAll('[data-layout]')){
  const panes=[...root.querySelector('.fg-panes').children].filter(n=>n.hasAttribute('data-pane'));
  const main=panes.find(n=>n.dataset.pane==='main');if(!main)continue;
  const local=selector=>[...root.querySelectorAll(selector)].filter(n=>n.closest('[data-layout]')===root);
  const state={root,grid:root.querySelector('.fg-panes'),panes,main,open:new Set(panes.map(n=>n.id)),sizes:new Map(panes.map(n=>[n.id,Number(n.dataset.default)])),active:main.id,narrow:matchMedia('(max-width:800px)'),focus:new Map(),context:new Map(),resetScroll:new Set()};layouts.set(root.id,state);
  for(const pane of panes)pane.addEventListener('focusin',e=>{if(e.target.closest('[data-layout]')===root)state.focus.set(pane.id,{node:e.target,key:focusKeyByElement.get(e.target)});});
  for(const n of local('[data-pane-toggle]'))n.addEventListener('click',()=>{const pane=panes.find(p=>p.id===n.dataset.paneToggle);if(pane)togglePane(state,pane);});
  for(const n of local('[data-pane-switch]'))n.addEventListener('click',()=>{
    const pane=panes.find(p=>p.id===n.dataset.paneSwitch);if(!pane)return;
    state.open.add(pane.id);state.active=pane.id;updateLayout(state);
    const target=state.focus.get(pane.id);if(target?.node.isConnected&&visible(target.node)&&(target.key===undefined||focusKeyByElement.get(target.node)===target.key))target.node.focus();else element(pane.id+'-heading')?.focus();
  });
  root.querySelector('[data-layout-reset]').addEventListener('click',()=>{state.open=new Set(panes.map(n=>n.id));state.sizes=new Map(panes.map(n=>[n.id,Number(n.dataset.default)]));state.active=main.id;updateLayout(state);});
  for(const separator of local('[data-resize-pane]')){
    const pane=panes.find(p=>p.id===separator.dataset.resizePane);const position=pane.dataset.pane;let drag=null;
    separator.addEventListener('keydown',e=>{
      if(e.altKey||e.ctrlKey||e.metaKey)return;
      const step=e.shiftKey?40:10;let size=state.sizes.get(pane.id);
      if(e.key==='Enter'){e.preventDefault();togglePane(state,pane);return;}
      if(e.key==='Home')size=Number(pane.dataset.min);else if(e.key==='End')size=Number(pane.dataset.max);
      else if(position==='bottom'&&(e.key==='ArrowUp'||e.key==='ArrowDown'))size+=e.key==='ArrowUp'?step:-step;
      else if(position!=='bottom'&&(e.key==='ArrowLeft'||e.key==='ArrowRight'))size+=(e.key==='ArrowRight'?step:-step)*(position==='trailing'?-1:1);else return;
      e.preventDefault();resizePane(state,pane,size);
    });
    separator.addEventListener('pointerdown',e=>{if(e.button!==0)return;drag={at:position==='bottom'?e.clientY:e.clientX,size:state.sizes.get(pane.id)};separator.setPointerCapture(e.pointerId);separator.focus();e.preventDefault();});
    separator.addEventListener('pointermove',e=>{if(!drag)return;const delta=(position==='bottom'?e.clientY:e.clientX)-drag.at;resizePane(state,pane,drag.size+delta*(position==='leading'?1:-1));});
    for(const name of ['pointerup','pointercancel','lostpointercapture'])separator.addEventListener(name,()=>{drag=null;});
  }
  state.narrow.addEventListener('change',()=>{const focus=document.activeElement;const pane=panes.find(p=>p.contains(focus));if(pane){state.active=pane.id;state.open.add(pane.id);}updateLayout(state);if(!visible(focus)&&focus!==document.body)element(state.active+'-switch')?.focus();});
  new ResizeObserver(()=>updateLayout(state)).observe(state.grid);
  updateLayout(state);
}
window.addEventListener('pagehide',stop);
window.addEventListener('pageshow',start);start();
"#;

pub(crate) const WORKER: &str = r#"'use strict';
// GENERATED reusable WASM/HTTP adapter. CONFIG is validated Rust declaration data.
const MAX=8*1024*1024;
const encoder=new TextEncoder();const decoder=new TextDecoder('utf-8',{fatal:true});
let wasm=null;let csrf=null;let closed=false;
const pending=new Map();
function field(value,path){for(const part of path.split('.')){if(value===null || typeof value!=='object' || !Object.hasOwn(value,part))throw new Error('Missing declared field');value=value[part];}if(typeof value!=='string' && typeof value!=='number')throw new Error('Invalid declared field');return value;}
function safeJson(text){let depth=0,quoted=false,escape=false;for(const c of text){if(quoted){if(escape)escape=false;else if(c==='\\')escape=true;else if(c==='"')quoted=false;}else if(c==='"')quoted=true;else if(c==='{'||c==='['){if(++depth>64)throw new Error('Nesting limit');}else if(c==='}'||c===']')depth--;}return JSON.parse(text);}
async function bounded(response,limit){const reader=response.body?.getReader();if(!reader)throw new Error('Missing response body');const chunks=[];let total=0;try{for(;;){const {value,done}=await reader.read();if(done)break;total+=value.length;if(total>limit){await reader.cancel();throw new Error('Byte limit');}chunks.push(value);}}finally{reader.releaseLock();}const bytes=new Uint8Array(total);let at=0;for(const c of chunks){bytes.set(c,at);at+=c.length;}return bytes;}
function dispatch(event,presentation_sequence=null){
  if(closed||!wasm)return;
  const input=encoder.encode(JSON.stringify({now_ms:Date.now(),event}));if(input.length>MAX)throw new Error('Input limit');
  const ptr=wasm.reserve_input(input.length);if(!ptr)throw new Error('Input allocation refused');
  new Uint8Array(wasm.memory.buffer,ptr,input.length).set(input);
  const status=wasm.dispatch_input();const len=wasm.output_len();const out=wasm.output_ptr();
  if(!status || !len || len>MAX || out+len>wasm.memory.buffer.byteLength)throw new Error('Invalid WASM result');
  let result;try{result=safeJson(decoder.decode(new Uint8Array(wasm.memory.buffer,out,len).slice()));}finally{wasm.release_output();}
  if(result.protocol_version!==1)throw new Error('Protocol mismatch');
  postMessage({kind:'view',view:result.view,presentation_sequence});
  for(const effect of result.effects)run(effect).catch(()=>fatal('Invalid application effect. Reload to retry.'));
}
function fatal(message){if(closed)return;closed=true;for(const c of pending.values())c.abort();pending.clear();csrf=null;wasm=null;postMessage({kind:'fatal',message});}
function failed(id,category){try{dispatch({kind:'network_failed',request_id:id,category});}catch(_){fatal('The worker could not accept a transport result. Reload to retry.');}}
async function run(effect){
  if(effect.kind==='abort'){const active=pending.get(effect.token.request_id);if(active)active.abort();return;}
  const binding=CONFIG.bindings.find(b=>b.kind===effect.kind);if(!binding)throw new Error('Undeclared transport effect');
  const id=String(field(effect,binding.request_id));if(pending.has(id)||pending.size>=2){failed(id,'limit');return;}
  let duration=5000;
  if(binding.deadline){duration=Math.min(duration,Number(field(effect,binding.deadline))-Date.now());if(duration<=0){failed(id,'timeout');return;}}
  const controller=new AbortController();pending.set(id,controller);let timedOut=false;
  const timer=setTimeout(()=>{timedOut=true;controller.abort();},duration);
  try{
    const headers={'Content-Type':'application/json'};
    for(const h of binding.headers)headers[h.name]=h.literal===null?String(field(effect,h.field)):h.literal;
    if(binding.csrf){if(!csrf){failed(id,'denied');return;}headers['Forge-CSRF']=csrf;}
    const body=JSON.stringify(effect);if(encoder.encode(body).length>16384)throw new Error('Request limit');
    const response=await fetch('/forge/v1/dispatch',{method:'POST',headers,body,credentials:'same-origin',redirect:'error',signal:controller.signal,cache:'no-store'});
    if(!response.ok){if(response.body)await response.body.cancel();failed(id,response.status===401||response.status===403?'denied':response.status===413?'limit':response.status===408||response.status===504?'timeout':response.status>=500?'unavailable':'invalid_response');return;}
    const bytes=await bounded(response,MAX);const decoded=safeJson(decoder.decode(bytes));
    if(binding.deadline && Date.now()>Number(field(effect,binding.deadline))){failed(id,'timeout');return;}
    if(decoded.protocol_version!==1 || !decoded.event || !binding.accept_events.includes(decoded.event.kind))throw new Error('Unexpected response envelope');
    if(decoded.csrf!==undefined && decoded.csrf!==null){if(typeof decoded.csrf!=='string'||!/^[-a-zA-Z0-9_]{16,128}$/.test(decoded.csrf))throw new Error('Invalid session metadata');csrf=decoded.csrf;}
    try{dispatch(decoded.event);}catch(_){fatal('Invalid application ABI result. Reload to retry.');}
  }catch(e){failed(id,timedOut?'timeout':controller.signal.aborted?'cancelled':e instanceof TypeError?'unavailable':'invalid_response');}
  finally{clearTimeout(timer);pending.delete(id);}
}
const ready=(async()=>{
  const response=await fetch('/app.wasm',{credentials:'same-origin',cache:'no-store'});if(!response.ok)throw new Error('WASM asset unavailable');
  const bytes=await bounded(response,MAX);const instance=await WebAssembly.instantiate(bytes,{});wasm=instance.instance.exports;
  for(const name of ['abi_version','reserve_input','dispatch_input','output_ptr','output_len','release_output'])if(typeof wasm[name]!=='function')throw new Error('Missing WASM export');
  if(wasm.abi_version()!==2 || !(wasm.memory instanceof WebAssembly.Memory))throw new Error('WASM ABI mismatch');
  dispatch({kind:CONFIG.initial_event});
})();
ready.catch(()=>fatal('Could not start the verified Rust WASM application. Reload to retry.'));
onmessage=async({data})=>{if(closed)return;try{
  await ready;
  if(data&&typeof data==='object'&&Object.hasOwn(data,'presentation_sequence')){
    if(Object.keys(data).length!==2||!Object.hasOwn(data,'event')||!Number.isSafeInteger(data.presentation_sequence)||data.presentation_sequence<1||data.presentation_sequence>2147483647)throw new Error('Invalid presentation envelope');
    dispatch(data.event,data.presentation_sequence);
  }else dispatch(data);
}catch(_){fatal('The worker rejected this application message. Reload to retry.');}};
"#;

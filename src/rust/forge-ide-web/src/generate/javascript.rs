//! Audited reusable browser-control and transport emitters. These primitives
//! never know IDE node names, report algorithms or application state transitions.
//! Typed application declarations configure controls and HTTP field projection.
pub(crate) const MAIN: &str = r#"'use strict';
// GENERATED reusable DOM primitive. Application behavior lives in worker WASM.
let worker = null;
const actionByElement = new WeakMap();
function tell(event) { if (worker) worker.postMessage(event); }
function fatal(message) { const n=document.querySelector('[role="status"]'); if(n) n.textContent=message; }
function element(id) { return typeof id==='string' ? document.getElementById(id) : null; }
function apply(view) {
  if (!view || typeof view!=='object') throw new Error('Invalid view');
  const focus=document.activeElement; const focusKey=focus?.dataset?.focusKey;
  for (const b of view.visibility || []) { const n=element(b.id); if(n)n.hidden=!b.visible; }
  for (const b of view.texts || []) { const n=element(b.id);if(n)n.textContent=b.text; }
  for (const b of view.buttons || []) { const n=element(b.id);if(n)n.disabled=b.disabled; }
  for (const b of view.choices || []) {
    const n=element(b.id);if(!n)continue;
    const signature=JSON.stringify(b.options);
    if(n.dataset.options!==signature){
      n.replaceChildren(...b.options.map(o=>{const x=document.createElement('option');x.value=o.value;x.textContent=o.label;return x;}));
      n.dataset.options=signature;
    }
    n.value=b.value;n.disabled=b.disabled;
  }
  for (const b of view.lists || []) {
    const n=element(b.id);if(!n)continue;
    const prior=new Map([...n.children].filter(x=>x.dataset.rowId).map(x=>[x.dataset.rowId,x]));
    const keep=new Set();
    for(const row of b.rows){
      const key=String(row.id);keep.add(key);let item=prior.get(key);
      if(!item){
        item=document.createElement('div');item.dataset.rowId=key;
        const toggle=document.createElement('button');toggle.type='button';toggle.className='fg-toggle';
        const select=document.createElement('button');select.type='button';select.className='fg-row-select';select.setAttribute('role','treeitem');
        const label=document.createElement('span');label.className='fg-row-label';
        const secondary=document.createElement('span');secondary.className='fg-row-secondary';select.append(label,secondary);item.append(toggle,select);
        toggle.addEventListener('click',()=>{const a=actionByElement.get(toggle);if(a)tell(a);});
        select.addEventListener('click',()=>{const a=actionByElement.get(select);if(a)tell(a);});
      }
      item.className='fg-tree-row fg-depth-'+Math.min(64,Math.max(0,Number(row.depth)||0));item.dataset.selected=String(row.selected);
      const [toggle,select]=item.children;
      toggle.hidden=row.expanded===null;toggle.textContent=row.expanded?'▾':'▸';toggle.setAttribute('aria-label',(row.expanded?'Collapse ':'Expand ')+row.label);
      toggle.dataset.focusKey=b.id+':'+key+':toggle';select.dataset.focusKey=b.id+':'+key+':select';
      actionByElement.set(toggle,row.toggle);actionByElement.set(select,row.action);
      select.setAttribute('aria-level',String(row.depth+1));select.setAttribute('aria-selected',String(row.selected));
      if(row.expanded===null)select.removeAttribute('aria-expanded');else select.setAttribute('aria-expanded',String(row.expanded));
      select.children[0].textContent=row.label;select.children[1].textContent=row.secondary;select.title=row.label;
      n.append(item);
    }
    for(const x of [...n.children])if(!x.dataset.rowId || !keep.has(x.dataset.rowId))x.remove();
    if(!b.rows.length){const p=document.createElement('p');p.className='fg-empty';p.textContent=b.empty_message;n.append(p);}
  }
  for(const b of view.tables || []){
    const n=element(b.id);if(!n)continue;
    const parts=[];for(const row of b.rows){const t=document.createElement('dt');t.textContent=row.label;const d=document.createElement('dd');d.textContent=row.value;parts.push(t,d);}n.replaceChildren(...parts);
  }
  if(focusKey){const same=[...document.querySelectorAll('[data-focus-key]')].find(n=>n.dataset.focusKey===focusKey);if(same && !same.closest('[hidden]'))same.focus({preventScroll:true});}
}
function start(){
  if(worker)return;
  try{worker=new Worker('/worker.js');worker.onmessage=({data})=>{try{if(data.kind==='view')apply(data.view);else if(data.kind==='fatal')fatal(data.message);}catch(_){fatal('Could not display the checked view. Reload to retry.');}};worker.onerror=()=>fatal('The Rust worker could not start. Reload to retry.');}
  catch(_){fatal('This browser cannot start the required worker.');}
}
for(const n of document.querySelectorAll('select[data-action]'))n.addEventListener('change',()=>tell({kind:n.dataset.action,value:n.value}));
for(const n of document.querySelectorAll('button[data-action]'))n.addEventListener('click',()=>tell({kind:n.dataset.action}));
for(const n of document.querySelectorAll('form[data-action]'))n.addEventListener('submit',e=>{e.preventDefault();const input=n.querySelector('input[name="value"]');if(!input)return;const value=input.value;input.value='';tell({kind:n.dataset.action,value});});
for(const tree of document.querySelectorAll('[role="tree"]'))tree.addEventListener('keydown',e=>{
  const rows=[...tree.querySelectorAll('.fg-row-select')];const index=rows.indexOf(document.activeElement);if(index<0)return;
  let next=index;
  if(e.key==='ArrowDown')next=Math.min(index+1,rows.length-1);else if(e.key==='ArrowUp')next=Math.max(index-1,0);else if(e.key==='Home')next=0;else if(e.key==='End')next=rows.length-1;
  else if(e.key==='ArrowRight'||e.key==='ArrowLeft'){const n=rows[index];const want=e.key==='ArrowRight';if(n.hasAttribute('aria-expanded') && (n.getAttribute('aria-expanded')==='true')!==want){const a=actionByElement.get(n.parentElement.children[0]);if(a)tell(a);}e.preventDefault();return;}else return;
  e.preventDefault();rows[next]?.focus();
});
window.addEventListener('pagehide',()=>{if(worker){worker.terminate();worker=null;}});
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
function dispatch(event){
  if(closed||!wasm)return;
  const input=encoder.encode(JSON.stringify({now_ms:Date.now(),event}));if(input.length>MAX)throw new Error('Input limit');
  const ptr=wasm.reserve_input(input.length);if(!ptr)throw new Error('Input allocation refused');
  new Uint8Array(wasm.memory.buffer,ptr,input.length).set(input);
  const status=wasm.dispatch_input();const len=wasm.output_len();const out=wasm.output_ptr();
  if(!status || !len || len>MAX || out+len>wasm.memory.buffer.byteLength)throw new Error('Invalid WASM result');
  let result;try{result=safeJson(decoder.decode(new Uint8Array(wasm.memory.buffer,out,len).slice()));}finally{wasm.release_output();}
  if(result.protocol_version!==1)throw new Error('Protocol mismatch');
  postMessage({kind:'view',view:result.view});
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
  if(wasm.abi_version()!==1 || !(wasm.memory instanceof WebAssembly.Memory))throw new Error('WASM ABI mismatch');
  dispatch({kind:CONFIG.initial_event});
})();
ready.catch(()=>fatal('Could not start the verified Rust WASM application. Reload to retry.'));
onmessage=async({data})=>{if(closed)return;try{await ready;dispatch(data);}catch(_){fatal('The worker rejected this application message. Reload to retry.');}};
"#;

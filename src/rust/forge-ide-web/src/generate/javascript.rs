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
let sourceViews=new WeakMap();
let transcriptViews=new WeakMap();
let presentationSequence=0;
let pendingActionOrigin=null;
let workspaceWork=false;
let leaveWarning=false;
let leaving=false;
let finishReload=null;
let navigation=null;
let navigationId=0;
const NAVIGATION_CACHE_KEY='forge-workspace-navigation';
const initialText=new Map([...document.querySelectorAll('[data-bind-text]')].map(n=>[n,n.textContent]));
const draftFields=[...document.querySelectorAll('textarea[data-action],[data-workspace-draft]')];
const leaveDialog=document.getElementById('leave-confirmation');
const dialogs=new Map([...document.querySelectorAll('[data-modal]')].map(n=>[n.id,n]));
const panelToggles=new Map([...document.querySelectorAll('button[data-action][aria-expanded][aria-controls]')].map(n=>[n.getAttribute('aria-controls'),n]));
const layouts=new Map();
const sourceFormats=new Map();
let typeahead=new WeakMap();
function element(id){return typeof id==='string'?document.getElementById(id):null;}
function tabKey(){
  try{
    const key='forge-workspace-tab';
    const saved=sessionStorage.getItem(key);
    if(saved&&/^[0-9a-f]{32}$/.test(saved))return saved;
    const tab=Array.from(crypto.getRandomValues(new Uint8Array(16)),b=>b.toString(16).padStart(2,'0')).join('');
    sessionStorage.setItem(key,tab);
    return tab;
  }catch(_){return '';}
}
function warnBeforeUnload(event){event.preventDefault();event.returnValue=true;}
function hasWork(){return workspaceWork||draftFields.some(n=>n.value.trim().length>0);}
function syncLeaveWarning(){
  const enabled=!leaving&&hasWork();
  if(enabled===leaveWarning)return;
  leaveWarning=enabled;
  if(enabled)window.addEventListener('beforeunload',warnBeforeUnload);
  else window.removeEventListener('beforeunload',warnBeforeUnload);
}
function reloadPage(){
  if(leaving)return;
  leaving=true;
  syncLeaveWarning();
  if(!worker){location.reload();return;}
  const timer=setTimeout(()=>finishReload?.(),1250);
  finishReload=()=>{clearTimeout(timer);finishReload=null;location.reload();};
  worker.postMessage({flush_workspace:'reload'});
}
function readNavigationCache(){
  try{return sessionStorage.getItem(NAVIGATION_CACHE_KEY);}catch(_){return null;}
}
function writeNavigationCache(cache){
  try{
    if(typeof cache==='string')sessionStorage.setItem(NAVIGATION_CACHE_KEY,cache);
    else sessionStorage.removeItem(NAVIGATION_CACHE_KEY);
  }catch(_){}
}
function navigationStatus(message){
  const status=document.querySelector('[data-workspace-navigation-status]');
  if(status){status.textContent=message;status.hidden=!message;}
}
function finishNavigation(result){
  if(!navigation||result.id!==navigation.id)return;
  const {url,sequence,timer}=navigation;
  clearTimeout(timer);navigation=null;
  if(!result.saved||sequence!==presentationSequence||draftFields.some(n=>inputDrafts.get(n)?.invalid)){
    navigationStatus('Your latest workspace changes are still open here. Try Design again.');
    return;
  }
  writeNavigationCache(result.cache);
  leaving=true;syncLeaveWarning();location.assign(url);
}
for(const link of document.querySelectorAll('a[data-workspace-navigation]'))link.addEventListener('click',event=>{
  if(event.defaultPrevented||event.button!==0||event.ctrlKey||event.metaKey||event.shiftKey||event.altKey)return;
  const url=new URL(link.href);
  if(url.origin!==location.origin)return;
  event.preventDefault();
  if(leaving||navigation)return;
  if(draftFields.some(n=>inputDrafts.get(n)?.invalid)){
    navigationStatus('Shorten the draft to the message limit before opening Design.');return;
  }
  if(!hasWork()){
    leaving=true;syncLeaveWarning();location.assign(url.href);return;
  }
  if(!worker){navigationStatus('Your workspace is unavailable. Reload and try Design again.');return;}
  const id=++navigationId;
  navigation={id,url:url.href,sequence:presentationSequence,timer:setTimeout(()=>finishNavigation({id,saved:false}),11000)};
  navigationStatus('Saving workspace…');
  worker.postMessage({flush_workspace:'navigation',id});
});
function visible(n){
  if(!n||!n.isConnected||n.closest('[hidden]'))return false;
  if(n.closest('[data-reader-layout]')&&!n.getClientRects().length)return false;
  for(let p=n.parentElement;p;p=p.parentElement){
    if(p.tagName==='DIALOG'&&!p.open)return false;
    if(p.tagName==='DETAILS'&&!p.open&&!p.querySelector(':scope > summary')?.contains(n))return false;
  }
  return true;
}
function tell(event,sequence){if(worker && !fatalState)worker.postMessage(sequence===undefined?event:{event,presentation_sequence:sequence});}
function sendAction(n,value,sequence){
  if(!visible(n)||n.disabled)return;
  const bound=actionByElement.get(n);
  let action=bound || (n.hasAttribute('data-dynamic-action')?null:{kind:n.dataset.action});
  if(!action?.kind||!worker||fatalState)return;
  if(sequence===undefined){
    if(presentationSequence>=2147483647){fatal('The presentation message limit was reached. Reload to retry.');return;}
    sequence=++presentationSequence;
  }
  const pane=n.closest('[data-pane]');
  const uuidField=n.dataset.uuidField;
  if(uuidField){
    const bytes=crypto.getRandomValues(new Uint8Array(16));
    bytes[6]=(bytes[6]&15)|64;bytes[8]=(bytes[8]&63)|128;
    const hex=Array.from(bytes,b=>b.toString(16).padStart(2,'0')).join('');
    action={...action,[uuidField]:[hex.slice(0,8),hex.slice(8,12),hex.slice(12,16),hex.slice(16,20),hex.slice(20)].join('-')};
  }
  tell(value===undefined?action:{...action,value},sequence);
  pendingActionOrigin={sequence,pane};
}
function setMarkdown(node,html){
  // Only sanitized HTML produced by the Rust Markdown renderer may pass here.
  node.innerHTML=html;
}
function showSourceFormat(format){
  const display=format.showDisplay&&format.html!==null;
  format.raw.hidden=display;
  format.preview.hidden=!display;
  format.rawButton.setAttribute('aria-pressed',String(!display));
  format.displayButton.setAttribute('aria-pressed',String(display));
  if(display&&format.rendered!==format.html){
    if(format.html==='')format.preview.textContent='Empty Markdown file.';
    else setMarkdown(format.preview,format.html);
    format.rendered=format.html;
    format.preview.scrollTop=0;
  }
}
function updateSourceFormat(format,context,markdown){
  const html=typeof markdown?.rendered_html==='string'&&markdown.rendered_html.length<=131072
    ?markdown.rendered_html:null;
  if(context!==format.context||(format.html!==null&&html===null)){
    format.context=context;
    format.showDisplay=false;
    format.rendered=null;
    format.preview.replaceChildren();
  }
  format.html=html;
  format.controls.hidden=!markdown;
  format.displayButton.disabled=html===null;
  format.unavailable.hidden=!markdown||html!==null;
  showSourceFormat(format);
}
function clearProtected(){
  // Synchronous: never leave catalog/report pixels behind on pagehide or bfcache restore.
  for(const n of document.querySelectorAll('[data-protected]'))n.hidden=true;
  for(const n of document.querySelectorAll('.fg-screen:not([data-protected])'))n.hidden=false;
  for(const [n,text] of initialText){n.textContent=text;delete n.dataset.tone;}
  for(const n of document.querySelectorAll('select[data-action]')){n.replaceChildren();n.value='';n.removeAttribute('title');delete n.dataset.options;n.disabled=true;}
  for(const n of document.querySelectorAll('input,textarea')){n.value='';n.setCustomValidity('');n.removeAttribute('aria-invalid');if(n.hasAttribute('data-action'))n.disabled=true;}
  for(const n of document.querySelectorAll('button[data-action],form[data-action] button'))n.disabled=true;
  for(const n of document.querySelectorAll('.fg-tree,.fg-details,[data-source-view],[data-transcript]')){n.replaceChildren();n.scrollTop=0;delete n.dataset.conversation;}
  for(const format of sourceFormats.values())updateSourceFormat(format,null,null);
  for(const n of document.querySelectorAll('[data-name-for]'))n.textContent='';
  for(const n of document.querySelectorAll('.fg-pane-body'))n.scrollTop=0;
  for(const n of document.querySelectorAll('.fg-disclosure'))n.open=false;
  for(const n of dialogs.values())if(n.open)n.close();
  for(const n of panelToggles.values())n.setAttribute('aria-expanded','false');
  actionByElement=new WeakMap();focusKeyByElement=new WeakMap();typeahead=new WeakMap();inputDrafts=new WeakMap();sourceViews=new WeakMap();transcriptViews=new WeakMap();presentationSequence=0;pendingActionOrigin=null;
  for(const state of layouts.values()){state.focus.clear();state.context.clear();state.reveal.clear();state.resetScroll.clear();}
  workspaceWork=false;
  syncLeaveWarning();
}
function stop(){const old=worker;worker=null;if(old)old.terminate();clearProtected();}
function fatal(message){
  if(fatalState)return;
  const priorWork=hasWork();
  fatalState=true;stop();
  workspaceWork=priorWork;syncLeaveWarning();
  document.querySelector('.fg-app').hidden=true;
  element('runtime-message').textContent=typeof message==='string'?message.slice(0,512):'Application stopped. Reload to retry.';
  element('runtime-recovery').hidden=false;
  element('runtime-reload').focus();
}
function focusRow(row){
  if(!row||!visible(row))return;
  const tree=row.closest('[role="tree"]');
  for(const n of tree.querySelectorAll('.fg-row-select'))n.tabIndex=n===row?0:-1;
  // Explicit keyboard tree navigation is newer user intent; automatic pane
  // restoration never passes here. Retain the pending dispatch's acknowledgement.
  if(pendingActionOrigin)pendingActionOrigin={sequence:pendingActionOrigin.sequence,pane:row.closest('[data-pane]')};
  row.focus();row.scrollIntoView({block:'nearest'});
}
function apply(view,acknowledgedSequence,hasWorkspaceWork){
  if(!view||typeof view!=='object')throw new Error('Invalid view');
  const focus=document.activeElement;
  const focusKey=focusKeyByElement.get(focus);
  const priorTree=focus?.closest?.('[role="tree"]');
  const priorPane=focus?.closest?.('[data-pane]');
  const origin=pendingActionOrigin;
  // Capture provenance before clearing its acknowledgement, including early
  // focus-return paths. Older views must not consume newer pending intent.
  if(origin?.sequence===acknowledgedSequence)pendingActionOrigin=null;
  let invalidatedFocus=null;
  const sourceTargets=[];
  const dialogBindings=[];
  for(const b of view.visibility||[]){
    const n=element(b.id);if(!n)continue;
    if(dialogs.has(b.id)){dialogBindings.push([n,b.visible]);continue;}
    n.hidden=!b.visible;
    panelToggles.get(b.id)?.setAttribute('aria-expanded',String(b.visible));
  }
  for(const b of view.texts||[]){const n=element(b.id);if(n){if(n.textContent!==b.text)n.textContent=b.text;n.dataset.tone=['neutral','accent','warning','error'].includes(b.tone)?b.tone:'neutral';}}
  for(const b of view.buttons||[]){const n=element(b.id);if(n){n.disabled=b.disabled;actionByElement.set(n,b.action);if(n.closest('.fg-navigation')){if(b.disabled)n.setAttribute('aria-current','page');else n.removeAttribute('aria-current');}}}
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
  workspaceWork=hasWorkspaceWork===true;
  syncLeaveWarning();
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
      item.className='fg-tree-row fg-depth-'+Math.min(64,Math.max(0,Number(row.depth)||0));item.dataset.selected=String(row.selected);item.dataset.parent=row.parent??'';item.dataset.icon=['folder','document'].includes(row.icon)?row.icon:'symbol';
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
  for(const b of view.transcripts||[]){
    const n=element(b.id);if(!n?.hasAttribute('data-transcript'))continue;
    const conversation=String(b.conversation);
    const follow=n.dataset.conversation!==conversation||n.scrollHeight-n.scrollTop-n.clientHeight<48;
    n.dataset.conversation=conversation;
    if(!b.messages.length){
      if(!n.firstChild?.classList.contains('fg-empty')){
        const empty=document.createElement('p');empty.className='fg-empty';empty.textContent='Send a message to start this conversation.';n.replaceChildren(empty);
      }
      continue;
    }
    const current=[...n.querySelectorAll('.fg-message')];
    for(const [i,message] of b.messages.entries()){
      let node=current[i];
      if(!node){
        node=document.createElement('article');node.className='fg-message';
        node.append(document.createElement('strong'),document.createElement('p'));
        if(i===0)n.replaceChildren(node);else n.append(node);
      }
      const role=message.role==='user'?'user':'assistant';
      node.dataset.role=role;
      node.children[0].textContent=role==='user'?'You':b.assistant_label??'Forge';
      const previous=node.children[1];
      const existingNotice=node.children[2];
      if(typeof message.notice==='string'){
        const notice=existingNotice||document.createElement('p');
        notice.className='fg-message-notice';
        if(notice.textContent!==message.notice)notice.textContent=message.notice;
        if(!existingNotice)node.append(notice);
      }else existingNotice?.remove();
      const html=role==='assistant'&&typeof message.rendered_html==='string'&&message.rendered_html;
      if(html){
        if(transcriptViews.get(node)===html)continue;
        const body=previous.tagName==='DIV'?previous:document.createElement('div');
        body.className='fg-message-body';
        setMarkdown(body,message.rendered_html);
        if(body!==previous)previous.replaceWith(body);
        transcriptViews.set(node,html);
      }else{
        transcriptViews.delete(node);
        const body=previous.tagName==='P'?previous:document.createElement('p');
        if(body.textContent!==message.content)body.textContent=message.content;
        if(body!==previous)previous.replaceWith(body);
      }
    }
    for(const stale of current.slice(b.messages.length))stale.remove();
    if(follow)n.scrollTop=n.scrollHeight;
  }
  for(const b of view.tables||[]){
    const n=element(b.id);if(!n)continue;
    const parts=[];for(const row of b.rows){const property=document.createElement('div');property.className='fg-property';const t=document.createElement('dt');t.textContent=row.label;const d=document.createElement('dd');d.textContent=row.value;property.append(t,d);parts.push(property);}n.replaceChildren(...parts);
  }
  // Source windows are selected and bounded by WASM. The renderer only creates
  // numbered safe-text lines and projects the accepted declaration span.
  for(const b of view.sources||[]){
    const n=element(b.id);if(!n||!n.hasAttribute('data-source-view'))continue;
    const context=JSON.stringify(b.context);const span=JSON.stringify(b.target_span);
    const format=sourceFormats.get(n);
    if(format){
      const fileContext=JSON.stringify([b.context?.report,b.context?.entry]);
      updateSourceFormat(format,fileContext,b.markdown);
    }
    const prior=sourceViews.get(n);
    if(prior&&prior.context===context&&prior.text===b.text&&prior.first===b.first_line&&prior.column===b.first_column&&prior.span===span)continue;
    if(typeof b.text!=='string'||b.text.length>65536||!Number.isSafeInteger(b.first_line)||b.first_line<1||!Number.isSafeInteger(b.first_column)||b.first_column<0)throw new Error('Invalid source window');
    const lines=b.text===''?[]:b.text.split('\n');
    if(lines.length>257)throw new Error('Source line window limit');
    // A trailing newline belongs to the preceding displayed line, rather than
    // manufacturing an additional empty line beyond the model's window.
    if(lines.length&&lines[lines.length-1]==='')lines.pop();
    const parts=[];let selected=null;
    for(let i=0;i<lines.length;i++){
      const line=b.first_line+i;const row=document.createElement('span');row.className='fg-source-line';row.dataset.line=String(line);
      const number=document.createElement('span');number.className='fg-source-number';number.tabIndex=-1;number.setAttribute('role','group');number.setAttribute('aria-label','Source line '+line+(i===0&&b.first_column>0?', continued':''));number.textContent=String(line)+(i===0&&b.first_column>0?'↪':'');
      const text=document.createElement('span');text.className='fg-source-text';text.textContent=lines[i];
      const target=b.target_span;const marked=target&&line>=target.start.line&&(line<target.end.line||(line===target.end.line&&target.end.column>0));
      row.dataset.highlight=String(Boolean(marked));if(marked&&!selected)selected=row;
      row.append(number,text);parts.push(row);
    }
    n.replaceChildren(...parts);n.scrollLeft=0;n.scrollTop=0;
    sourceViews.set(n,{context,text:b.text,first:b.first_line,column:b.first_column,span});
    if(parts.length)sourceTargets.push(selected||parts[0]);
  }
  for(const b of view.panes||[]){
    for(const state of layouts.values()){
      const pane=state.panes.find(p=>p.id===b.id);if(!pane)continue;
      const context=JSON.stringify(b.context);
      // Preserve only an acknowledged direct action (or newer pending direct
      // intent), not a pane switch that restored a stale row while waiting.
      const direct=origin?.pane===pane&&Number.isSafeInteger(acknowledgedSequence)&&acknowledgedSequence>0&&acknowledgedSequence<=origin.sequence;
      if(state.context.get(b.id)!==context){
        state.context.set(b.id,context);
        if(!direct){
          state.focus.delete(b.id);
          state.resetScroll.add(b.id);
          if(pane.contains(focus))invalidatedFocus=element(pane.id+'-heading');
        }
      }
      if(Number.isSafeInteger(b.reveal)&&b.reveal>0&&state.reveal.get(b.id)!==b.reveal){
        state.reveal.set(b.id,b.reveal);
        if(state.narrow.matches&&!direct){openPane(state,pane);state.active=pane.id;invalidatedFocus=element(pane.id+'-heading');}
      }
    }
  }
  for(const state of layouts.values())updateLayout(state);
  // A still-valid row in the destination pane owns keyboard/scroll intent.
  // Showing new source must not scroll that row out of view. External navigation
  // invalidates old pane focus above and is free to reveal the accepted source.
  let sourceFocus=null;
  for(const target of sourceTargets){
    const pane=target.closest('[data-pane]');
    const direct=origin?.pane===pane&&Number.isSafeInteger(acknowledgedSequence)&&acknowledgedSequence>0&&acknowledgedSequence<=origin.sequence;
    if(!visible(target)||(direct&&!focus.disabled)||(focusKey&&priorPane===pane&&!invalidatedFocus))continue;
    // The bounded gutter anchor remains visible even when a source line is wider
    // than the viewport. Focus follows the code instead of an offscreen heading.
    const source=target.closest('[data-source-view]');
    if(source&&target.dataset.highlight==='true'){
      // Put the declaration start at the top of this source window before
      // revealing it in outer scroll containers. Nearest alone can leave the
      // selected declaration at the bottom, showing mostly preceding code.
      source.scrollTop+=target.getBoundingClientRect().top-source.getBoundingClientRect().top-source.clientTop;
    }
    const anchor=target.children[0];anchor.scrollIntoView({block:'nearest',inline:'nearest'});
    const heading=pane?element(pane.id+'-heading'):null;
    if((heading&&(invalidatedFocus===heading||focus===heading))||(direct&&focus.disabled))sourceFocus=anchor;
  }
  const focusedDialog=focus?.closest?.('[data-modal]');
  for(const [n,show] of dialogBindings)if(!show&&n.open)n.close();
  for(const [n,show] of dialogBindings)if(show&&!n.open){
    n.showModal();
    element(n.dataset.initialFocus)?.focus();
    return;
  }
  if(focusedDialog&&!focusedDialog.open&&document.activeElement!==document.body&&visible(document.activeElement))return;
  if(visible(sourceFocus)){sourceFocus.focus({preventScroll:true});return;}
  if(visible(invalidatedFocus)){invalidatedFocus.focus();return;}
  if(focusKey){const same=[...document.querySelectorAll('.fg-row-select,.fg-toggle')].find(n=>focusKeyByElement.get(n)===focusKey);if(visible(same)){same.focus({preventScroll:true});return;}}
  // Only repair focus that this rendering invalidated. Background results do not steal it.
  if(focus!==document.body && (!focus.isConnected||!visible(focus))){
    let target=visible(priorTree)?priorTree.querySelector('.fg-row-select[tabindex="0"]')||priorTree:visible(priorPane)?element(priorPane.id+'-heading'):null;
    if(!target){
      const controls=[...document.querySelectorAll('.fg-screen:not([hidden]) :is(select,input,button):not(:disabled)')].filter(visible);
      target=controls.find(n=>n.classList.contains('fg-button-primary'))||controls[0];
    }
    if(visible(target))target.focus({preventScroll:true});
  }
}
function start(){
  if(worker||fatalState)return;
  leaving=false;
  clearProtected();
  try{
    const owned=new Worker('/worker.js',{name:tabKey()});worker=owned;
    owned.onmessage=({data})=>{
      if(worker!==owned||fatalState)return;
      try{
        if(data.kind==='view')apply(data.view,data.presentation_sequence,data.has_workspace_work);
        else if(data.kind==='fatal')fatal(data.message);
        else if(data.kind==='workspace_flushed')finishReload?.();
        else if(data.kind==='workspace_navigation_flushed')finishNavigation(data);
        else if(data.kind==='clear_navigation_cache')writeNavigationCache(null);
      }catch(_){fatal('Could not display the checked view. Reload to retry.');}
    };
    owned.onerror=()=>{if(worker===owned)fatal('The Rust worker could not start. Reload to retry.');};
    const cache=readNavigationCache();
    if(cache)owned.postMessage({workspace_navigation_cache:cache});
  }catch(_){fatal('This browser cannot start the required worker. Reload to retry.');}
}
for(const group of document.querySelectorAll('[data-source-formats]')){
  const format={
    controls:group.querySelector('[data-source-format]'),
    raw:group.querySelector('[data-source-raw]'),
    preview:group.querySelector('[data-source-preview]'),
    unavailable:group.querySelector('[data-source-preview-unavailable]'),
    rawButton:group.querySelector('[data-source-mode="raw"]'),
    displayButton:group.querySelector('[data-source-mode="display"]'),
    context:null,html:null,rendered:null,showDisplay:false,
  };
  sourceFormats.set(format.raw.querySelector('[data-source-view]'),format);
  format.rawButton.addEventListener('click',()=>{format.showDisplay=false;showSourceFormat(format);});
  format.displayButton.addEventListener('click',()=>{format.showDisplay=true;showSourceFormat(format);});
}
for(const n of document.querySelectorAll('select[data-action]'))n.addEventListener('change',()=>sendAction(n,n.value));
for(const n of document.querySelectorAll('button[data-action]'))n.addEventListener('click',()=>{
  const check=n.dataset.checkInput;
  if(!check||element(check)?.reportValidity())sendAction(n);
});
for(const n of document.querySelectorAll('input[data-action],textarea[data-action]'))n.addEventListener('input',()=>{
  // The declared byte cap is a codec bound, not a second query/filter implementation.
  if(!visible(n)||n.disabled||!actionByElement.get(n))return;
  if(draftFields.includes(n))syncLeaveWarning();
  const context=JSON.stringify(actionByElement.get(n));const max=Number(n.dataset.maxBytes)||256;
  if(new TextEncoder().encode(n.value).length>max){inputDrafts.set(n,{context,invalid:true});n.setCustomValidity('Use at most '+max+' UTF-8 bytes. This draft has not been applied.');n.setAttribute('aria-invalid','true');n.reportValidity();return;}
  if(presentationSequence>=2147483647){fatal('The presentation message limit was reached. Reload to retry.');return;}
  const sequence=++presentationSequence;inputDrafts.set(n,{context,sequence,invalid:false});
  n.setCustomValidity('');n.removeAttribute('aria-invalid');sendAction(n,n.value,sequence);
});
for(const n of dialogs.values())n.addEventListener('cancel',event=>{event.preventDefault();tell({kind:n.dataset.modal});});
for(const n of document.querySelectorAll('form[data-action]'))n.addEventListener('submit',e=>{e.preventDefault();const input=n.querySelector('input[name="value"]');if(!input||n.querySelector('button')?.disabled)return;const value=input.value;input.value='';sendAction(n,value);});
for(const n of document.querySelectorAll('[data-reload]'))n.addEventListener('click',()=>{
  if(hasWork()&&leaveDialog)leaveDialog.showModal();
  else reloadPage();
});
element('leave-stay')?.addEventListener('click',()=>leaveDialog.close());
element('leave-confirm')?.addEventListener('click',()=>{leaveDialog.close();reloadPage();});
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
    const tab=element(pane.id+'-switch');if(tab)tab.setAttribute('aria-pressed',String(state.narrow.matches?state.active===pane.id:open));
  }
}
function openPane(state,pane){
  if(state.singleSecondary&&(pane.dataset.pane==='trailing'||pane.dataset.pane==='bottom')){
    for(const other of state.panes)if(other!==pane&&(other.dataset.pane==='trailing'||other.dataset.pane==='bottom'))state.open.delete(other.id);
  }
  state.open.add(pane.id);
}
function defaultPanes(panes,singleSecondary){return new Set(panes.filter(p=>!singleSecondary||p.dataset.pane!=='bottom').map(p=>p.id));}
function togglePane(state,pane){
  const focused=pane.contains(document.activeElement)||document.activeElement===element(pane.id+'-separator');
  if(state.open.has(pane.id)){state.open.delete(pane.id);if(state.active===pane.id)state.active=state.main.id;}
  else {openPane(state,pane);if(state.narrow.matches)state.active=pane.id;}
  updateLayout(state);if(focused)paneControl(state,pane)?.focus();
}
function resizePane(state,pane,value){state.sizes.set(pane.id,Math.max(Number(pane.dataset.min),Math.min(Number(pane.dataset.max),Math.round(value))));updateLayout(state);}
for(const root of document.querySelectorAll('[data-layout]')){
  const panes=[...root.querySelector('.fg-panes').children].filter(n=>n.hasAttribute('data-pane'));
  const main=panes.find(n=>n.dataset.pane==='main');if(!main)continue;
  const local=selector=>[...root.querySelectorAll(selector)].filter(n=>n.closest('[data-layout]')===root);
  const singleSecondary=root.dataset.singleSecondary==='true';
  const state={root,grid:root.querySelector('.fg-panes'),panes,main,singleSecondary,open:defaultPanes(panes,singleSecondary),sizes:new Map(panes.map(n=>[n.id,Number(n.dataset.default)])),active:main.id,narrow:matchMedia('(max-width:800px)'),focus:new Map(),context:new Map(),reveal:new Map(),resetScroll:new Set()};layouts.set(root.id,state);
  for(const pane of panes)pane.addEventListener('focusin',e=>{if(e.target.closest('[data-layout]')===root)state.focus.set(pane.id,{node:e.target,key:focusKeyByElement.get(e.target)});});
  for(const n of local('[data-pane-reveal]'))n.addEventListener('click',()=>{
    const pane=panes.find(p=>p.id===n.dataset.paneReveal);if(!pane)return;
    openPane(state,pane);state.active=pane.id;updateLayout(state);
    const target=state.focus.get(pane.id);if(target?.node.isConnected&&visible(target.node)&&(target.key===undefined||focusKeyByElement.get(target.node)===target.key))target.node.focus();else element(pane.id+'-heading')?.focus();
  });
  for(const n of local('[data-pane-toggle]'))n.addEventListener('click',()=>{const pane=panes.find(p=>p.id===n.dataset.paneToggle);if(pane)togglePane(state,pane);});
  for(const n of local('[data-pane-switch]'))n.addEventListener('click',()=>{
    const pane=panes.find(p=>p.id===n.dataset.paneSwitch);if(!pane)return;
    if(!state.narrow.matches&&pane!==main){togglePane(state,pane);return;}
    openPane(state,pane);state.active=pane.id;updateLayout(state);
    const target=state.focus.get(pane.id);if(target?.node.isConnected&&visible(target.node)&&(target.key===undefined||focusKeyByElement.get(target.node)===target.key))target.node.focus();else element(pane.id+'-heading')?.focus();
  });
  root.querySelector('[data-layout-reset]').addEventListener('click',()=>{state.open=defaultPanes(panes,state.singleSecondary);state.sizes=new Map(panes.map(n=>[n.id,Number(n.dataset.default)]));state.active=main.id;updateLayout(state);});
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
  state.narrow.addEventListener('change',()=>{const focus=document.activeElement;const pane=panes.find(p=>p.contains(focus));if(pane){state.active=pane.id;openPane(state,pane);}updateLayout(state);if(!visible(focus)&&focus!==document.body)element(state.active+'-switch')?.focus();});
  new ResizeObserver(()=>updateLayout(state)).observe(state.grid);
  updateLayout(state);
}
document.addEventListener('visibilitychange',()=>{
  if(document.visibilityState==='hidden'&&!leaving)worker?.postMessage({flush_workspace:'background'});
});
window.addEventListener('pagehide',()=>{
  finishReload=null;
  if(navigation){clearTimeout(navigation.timer);navigation=null;}
  navigationStatus('');
  if(leaveDialog?.open)leaveDialog.close();
  stop();
});
window.addEventListener('pageshow',start);start();
"#;

pub(crate) const WORKER: &str = r#"'use strict';
// GENERATED reusable WASM/HTTP adapter. CONFIG is validated Rust declaration data.
const MAX=8*1024*1024;
const WORKSPACE_MAX=2*1024*1024;
const NAVIGATION_CACHE_MAX=256*1024;
const encoder=new TextEncoder();const decoder=new TextDecoder('utf-8',{fatal:true});
let wasm=null;let csrf=null;let closed=false;
const workspaceTab=typeof name==='string'&&/^[0-9a-f]{32}$/.test(name)?name:null;
let workspaceLease=null;
let workspaceRevision=0;
let workspaceJSON=null;
let latestCheckpoint=null;
let latestWorkspace=null;
let mailNavigation=null;
let mailNavigationLocked=false;
let mailWatchTimer=null;
let workspaceEpoch=null;
let navigationCache=null;
const pending=new Map();
function field(value,path){for(const part of path.split('.')){if(value===null || typeof value!=='object' || !Object.hasOwn(value,part))throw new Error('Missing declared field');value=value[part];}if(typeof value!=='string' && typeof value!=='number')throw new Error('Invalid declared field');return value;}
function safeJson(text){let depth=0,quoted=false,escape=false;for(const c of text){if(quoted){if(escape)escape=false;else if(c==='\\')escape=true;else if(c==='"')quoted=false;}else if(c==='"')quoted=true;else if(c==='{'||c==='['){if(++depth>64)throw new Error('Nesting limit');}else if(c==='}'||c===']')depth--;}return JSON.parse(text);}
async function bounded(response,limit){const reader=response.body?.getReader();if(!reader)throw new Error('Missing response body');const chunks=[];let total=0;try{for(;;){const {value,done}=await reader.read();if(done)break;total+=value.length;if(total>limit){await reader.cancel();throw new Error('Byte limit');}chunks.push(value);}}finally{reader.releaseLock();}const bytes=new Uint8Array(total);let at=0;for(const c of chunks){bytes.set(c,at);at+=c.length;}return bytes;}
async function workspaceRequest(body,keepalive,signal){
  return fetch('/forge/v1/dispatch/workspace',{
    method:'POST',headers:{'Content-Type':'application/json','Forge-CSRF':csrf},
    body,keepalive,signal,credentials:'same-origin',redirect:'error',cache:'no-store'
  });
}
async function restoreWorkspace(chatHistory,chatModels){
  if(!workspaceTab||!csrf)return {snapshot:null,unavailable:false};
  const controller=new AbortController();
  const timer=setTimeout(()=>controller.abort(),2500);
  try{
    const request={operation:'restore',tab:workspaceTab,...(chatHistory?{history:true}:{}),...(chatModels?{models:true}:{})};
    let response=await workspaceRequest(JSON.stringify(request),false,controller.signal);
    if(chatModels&&response.status===400){
      await response.body?.cancel();
      delete request.models;
      response=await workspaceRequest(JSON.stringify(request),false,controller.signal);
    }
    if(!response.ok){await response.body?.cancel();return {snapshot:null,unavailable:response.status>=500};}
    const saved=safeJson(decoder.decode(await bounded(response,WORKSPACE_MAX)));
    if(!/^[0-9a-f]{64}$/.test(saved.lease))return {snapshot:null,unavailable:false};
    workspaceLease=saved.lease;
    workspaceJSON=saved.snapshot?JSON.stringify(saved.snapshot):null;
    return {snapshot:saved.snapshot??null,unavailable:false};
  }catch(_){return {snapshot:null,unavailable:true};}finally{clearTimeout(timer);}
}
async function saveCheckpoint(checkpoint){
  const controller=new AbortController();
  const timer=setTimeout(()=>controller.abort(),5000);
  try{
    const {body,bytes}=checkpoint;
    let response;
    try{
      response=await workspaceRequest(body,bytes<=60*1024,controller.signal);
    }catch(error){
      if(bytes>60*1024||controller.signal.aborted)throw error;
      response=await workspaceRequest(body,false,controller.signal);
    }
    checkpoint.saved=response.ok;
    await response.body?.cancel();
  }catch(_){}finally{clearTimeout(timer);}
}
function checkpointWorkspace(snapshot){
  latestWorkspace=snapshot;
  if(!workspaceLease||!csrf||!snapshot)return;
  const json=JSON.stringify(snapshot);
  if(json===workspaceJSON)return;
  const body=JSON.stringify({operation:'save',tab:workspaceTab,lease:workspaceLease,revision:++workspaceRevision,snapshot});
  const bytes=encoder.encode(body).length;
  if(bytes>WORKSPACE_MAX)return;
  workspaceJSON=json;
  const checkpoint={body,bytes,saved:false,pending:null};
  checkpoint.pending=saveCheckpoint(checkpoint);
  latestCheckpoint=checkpoint;
}
async function flushWorkspace(notify){
  const checkpoint=latestCheckpoint;
  if(checkpoint){
    await checkpoint.pending;
    if(!checkpoint.saved&&checkpoint===latestCheckpoint){
      checkpoint.pending=saveCheckpoint(checkpoint);
      await checkpoint.pending;
    }
  }
  if(notify)postMessage({kind:'workspace_flushed'});
}
async function flushNavigation(id){
  const snapshot=latestWorkspace;
  const json=JSON.stringify(snapshot);
  const checkpoint=latestCheckpoint;
  await flushWorkspace(false);
  // Only leave once the canonical checkpoint exactly matches this handoff.
  const saved=Boolean(workspaceLease&&snapshot&&workspaceJSON===json&&
    JSON.stringify(latestWorkspace)===json&&(!checkpoint||checkpoint.saved));
  let cache=null;
  if(saved&&workspaceEpoch&&!mailNavigationLocked){
    const local=JSON.stringify({version:1,tab:workspaceTab,epoch:workspaceEpoch,snapshot:navigationSnapshot(snapshot)});
    if(encoder.encode(local).length<=NAVIGATION_CACHE_MAX)cache=local;
  }
  postMessage({kind:'workspace_navigation_flushed',id,saved:saved&&!mailNavigationLocked&&(!mailNavigation||cache!==null),cache});
}
function navigationSnapshot(snapshot){
  // Keep draft text and selection hints. Rust validates the restored snapshot.
  const chat=snapshot?.chat;
  if(!Array.isArray(chat?.conversations)||chat.conversations.length>16)return null;
  const conversations=chat.conversations.map(item=>({draft:typeof item?.draft==='string'?item.draft:'',messages:[]}));
  return {chat:{selected:chat.selected,model_id:chat.model_id,conversations},
    workbench:snapshot.workbench,workbench_open:snapshot.workbench_open};
}
async function checkpointedRun(effect){
  if(!workspaceLease||!workspaceJSON)return false;
  const intent=safeJson(workspaceJSON).chat?.pending_run;
  if(intent?.chat_id!==effect.chat_id||intent.idempotency_key!==effect.idempotency_key||intent.model_id!==effect.model_id)return false;
  const checkpoint=latestCheckpoint;
  if(!checkpoint)return true; // Restored from a durable checkpoint.
  await checkpoint.pending;
  if(!checkpoint.saved&&checkpoint===latestCheckpoint){
    checkpoint.pending=saveCheckpoint(checkpoint);
    await checkpoint.pending;
  }
  return checkpoint.saved;
}
function dispatch(event,presentation_sequence=null,suppressView=false){
  if(closed||!wasm)return;
  const input=encoder.encode(JSON.stringify({now_ms:Date.now(),event}));if(input.length>MAX)throw new Error('Input limit');
  const ptr=wasm.reserve_input(input.length);if(!ptr)throw new Error('Input allocation refused');
  new Uint8Array(wasm.memory.buffer,ptr,input.length).set(input);
  const status=wasm.dispatch_input();const len=wasm.output_len();const out=wasm.output_ptr();
  if(!status || !len || len>MAX || out+len>wasm.memory.buffer.byteLength)throw new Error('Invalid WASM result');
  let result;try{result=safeJson(decoder.decode(new Uint8Array(wasm.memory.buffer,out,len).slice()));}finally{wasm.release_output();}
  if(result.protocol_version!==1)throw new Error('Protocol mismatch');
  if(!suppressView){
    mailNavigation=result.mail_navigation??null;
    mailNavigationLocked=result.mail_navigation_locked===true;
    postMessage({kind:'view',view:result.view,presentation_sequence,has_workspace_work:result.has_workspace_work});
    checkpointWorkspace(result.workspace);
  }
  for(const effect of result.effects)run(effect).catch(()=>fatal('Invalid application effect. Reload to retry.'));
}
function fatal(message){if(closed)return;closed=true;for(const c of pending.values())c.abort();pending.clear();csrf=null;wasm=null;postMessage({kind:'fatal',message});}
function failed(id,category){try{dispatch({kind:'network_failed',request_id:id,category});}catch(_){fatal('The worker could not accept a transport result. Reload to retry.');}}
async function run(effect){
  if(effect.kind==='abort'){const active=pending.get(effect.token.request_id);if(active)active.abort();return;}
  if(effect.kind==='mail_watch'){
    clearTimeout(mailWatchTimer);
    mailWatchTimer=setTimeout(()=>{if(!closed)dispatch({kind:'poll_mail',watch:effect.watch});},5000);
    return;
  }
  const binding=CONFIG.bindings.find(b=>b.kind===effect.kind);if(!binding)throw new Error('Undeclared transport effect');
  const id=String(field(effect,binding.request_id));
  // Run pages are short requests so they can share the host with inspection and history.
  if(binding.kind==='chat_run_events')await new Promise(resolve=>setTimeout(resolve,1200));
  if(binding.kind==='chat_run_submit'&&!await checkpointedRun(effect)){failed(id,'unavailable');return;}
  if(binding.kind==='mail_list'||binding.kind==='mail_open'){
    // A Mail handoff can restore while inspection and model discovery start.
    const slotDeadline=Date.now()+5000;
    while(!closed&&pending.size>=2&&Date.now()<slotDeadline)await new Promise(resolve=>setTimeout(resolve,100));
  }
  if(closed)return;
  if(pending.has(id)||pending.size>=2){failed(id,'limit');return;}
  let duration=binding.timeout_ms??5000;
  if(binding.deadline){duration=Math.min(duration,Number(field(effect,binding.deadline))-Date.now());if(duration<=0){failed(id,'timeout');return;}}
  const controller=new AbortController();pending.set(id,controller);let timedOut=false;
  const timer=setTimeout(()=>{timedOut=true;controller.abort();},duration);
  try{
    const headers={'Content-Type':'application/json'};
    for(const h of binding.headers)headers[h.name]=h.literal===null?String(field(effect,h.field)):h.literal;
    if(binding.csrf){if(!csrf){failed(id,'denied');return;}headers['Forge-CSRF']=csrf;}
    const body=JSON.stringify(effect);if(encoder.encode(body).length>16384)throw new Error('Request limit');
    const response=await fetch('/forge/v1/dispatch',{method:'POST',headers,body,credentials:'same-origin',redirect:'error',signal:controller.signal,cache:'no-store'});
    const typedErrors=binding.error_events??[];
    if((binding.kind==='chat'||typedErrors.length)&&(response.status===401||response.status===403)){
      if(response.body)await response.body.cancel();
      fatal('Your Forge session expired. Reload to reconnect.');return;
    }
    if(!response.ok&&(!typedErrors.length||!(response.status>=400&&response.status<500||response.status===503))){
      if(response.body)await response.body.cancel();
      failed(id,response.status===401||response.status===403?'denied':response.status===413?'limit':response.status===408||response.status===504?'timeout':response.status>=500?'unavailable':'invalid_response');return;
    }
    const bytes=await bounded(response,response.ok?MAX:16384);const decoded=safeJson(decoder.decode(bytes));
    if(binding.deadline && Date.now()>Number(field(effect,binding.deadline))){failed(id,'timeout');return;}
    const events=response.ok?binding.accept_events:typedErrors;
    if(decoded.protocol_version!==1||!decoded.event||!events.includes(decoded.event.kind)||(!response.ok&&decoded.event.request_id!==id))throw new Error('Unexpected response envelope');
    let saved=null;
    let savedMail=null;
    if(decoded.csrf!==undefined && decoded.csrf!==null){
      if(typeof decoded.csrf!=='string'||!/^[-a-zA-Z0-9_]{16,128}$/.test(decoded.csrf))throw new Error('Invalid session metadata');
      csrf=decoded.csrf;
      workspaceEpoch=decoded.event.catalog?.session_epoch??null;
      const restore=await restoreWorkspace(decoded.event.catalog?.chat_history===true,decoded.event.catalog?.async_chat===true);
      const cache=navigationCache;
      navigationCache=null;
      if(restore.unavailable&&cache?.version===1&&cache.tab===workspaceTab&&workspaceEpoch&&cache.epoch===workspaceEpoch){
        saved=navigationSnapshot(cache.snapshot);
      }else{
        saved=restore.snapshot;
        postMessage({kind:'clear_navigation_cache'});
      }
    }
    try{
      // The completed request should not occupy a slot used by its effects.
      pending.delete(id);
      dispatch(decoded.event,null,saved!==null);
      if(saved!==null)dispatch({kind:'restore_workspace',snapshot:saved});
      if(savedMail!==null)dispatch({kind:'restore_mail_navigation',snapshot:savedMail});
    }catch(_){fatal('Invalid application ABI result. Reload to retry.');}
  }catch(e){failed(id,timedOut?'timeout':controller.signal.aborted?'cancelled':e instanceof TypeError?'unavailable':'invalid_response');}
  finally{clearTimeout(timer);pending.delete(id);}
}
const ready=(async()=>{
  const response=await fetch('/app.wasm',{credentials:'same-origin',cache:'no-store'});if(!response.ok)throw new Error('WASM asset unavailable');
  const bytes=await bounded(response,MAX);const instance=await WebAssembly.instantiate(bytes,{});wasm=instance.instance.exports;
  for(const name of ['abi_version','reserve_input','dispatch_input','output_ptr','output_len','release_output'])if(typeof wasm[name]!=='function')throw new Error('Missing WASM export');
  if(wasm.abi_version()!==4 || !(wasm.memory instanceof WebAssembly.Memory))throw new Error('WASM ABI mismatch');
  dispatch({kind:CONFIG.initial_event});
})();
ready.catch(()=>fatal('Could not start the verified Rust WASM application. Reload to retry.'));
onmessage=async({data})=>{if(closed)return;try{
  if(typeof data?.workspace_navigation_cache==='string'){
    const text=data.workspace_navigation_cache;
    if(text.length<=NAVIGATION_CACHE_MAX&&encoder.encode(text).length<=NAVIGATION_CACHE_MAX){
      try{navigationCache=safeJson(text);}catch(_){}
    }
    return;
  }
  await ready;
  if(data?.flush_workspace==='reload'||data?.flush_workspace==='background')await flushWorkspace(data.flush_workspace==='reload');
  else if(data?.flush_workspace==='navigation'&&Number.isSafeInteger(data.id))await flushNavigation(data.id);
  else if(data&&typeof data==='object'&&Object.hasOwn(data,'presentation_sequence')){
    if(Object.keys(data).length!==2||!Object.hasOwn(data,'event')||!Number.isSafeInteger(data.presentation_sequence)||data.presentation_sequence<1||data.presentation_sequence>2147483647)throw new Error('Invalid presentation envelope');
    dispatch(data.event,data.presentation_sequence);
  }else dispatch(data);
}catch(_){fatal('The worker rejected this application message. Reload to retry.');}};
"#;

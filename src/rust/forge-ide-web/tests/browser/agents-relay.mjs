// Owned deterministic runner stand-in; never invokes a real coding agent.
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const time = '2026-11-19T18:00:00Z';
const project = 'workspace';
const base = 'a'.repeat(40);
const externalWorkflowPolicy={policy_digest:'d'.repeat(64),author_profile:'muse-db-primary',reviewer_profile:'codex-primary',max_plan_revisions:20,max_code_revisions:20,auto_merge:true};
const workflowPolicy={policy_digest:'c'.repeat(64),author_profile:'agy-primary',reviewer_profile:'codex-primary',max_plan_revisions:20,max_code_revisions:20,auto_merge:true};
const publicPlan = '## Public fixture plan\n\n1. Inspect approved files.\n2. Implement the change.\n3. Verify the result.\n\nLiteral <script>window.__forgeProgressExecuted = true</script> stays text.';
const publicMilestone = 'Public fixture milestone: implementation complete; running verification. <script>window.__forgeProgressExecuted = true</script>';
const foreignProgress = 'FOREIGN ATTEMPT PROGRESS MUST NOT APPEAR';
const resultText = '## Fixture agent result\n\nVerified first section.\n\n'
  + 'A readable fixture paragraph with élève and safe <script> text.\n\n'.repeat(1500)
  + '\nFinal fixture marker.\n';
const bytes = Buffer.from(resultText);
const digest = createHash('sha256').update(bytes).digest('hex');
const profiles = [
  { alias: 'agy-primary', label: 'Gemini', backend: 'antigravity', model: 'Gemini fixture', modes: ['plan', 'review', 'implement'], branch: 'forge/agent/agy-primary' },
  { alias: 'codex-primary', label: 'Codex', backend: 'codex', model: 'Codex fixture', modes: ['plan', 'review', 'implement'], branch: 'forge/agent/codex-primary' },
  { alias: 'grok-primary', label: 'Grok', backend: 'grok', model: 'grok-4.7', modes: ['review'], branch: 'forge/agent/grok-primary' },
];
function defaultAgentBranches() {
  return profiles.map((p, idx) => ({
    project_alias: project,
    agent_profile: p.alias,
    branch_name: p.branch,
    branch_scope: `branch/forge-agent-${p.alias}-0123456789abcde${idx}`,
    head_commit_sha: 'e'.repeat(40),
    fetched_main_sha: base,
    fetched_main_at: time,
    sync_status: 'synced_with_main',
    conflict_paths: [],
    cache_status: idx === 0 ? 'hit_branch' : 'hit_main',
    cache_updated_at: time,
    last_task_id: null,
    last_correlation_id: null,
    reservation_status: 'idle',
    active_scope_kind: null,
    active_scope_id: null,
    active_root_task_id: null,
    active_task_id: null,
    active_pr_url: null,
    reservation_epoch: idx,
    reservation_blocker: null,
  }));
}

function task(status, number, input = {}) {
  const taskId = number === 0 ? '11111111-1111-4111-8111-111111111111' : randomUUID();
  const item = {
    task_id: taskId, correlation_id:randomUUID(), project_alias: project, base_revision: base, mode: input.mode ?? 'review',
    agent_profile: input.agent_profile ?? 'agy-primary', recipe_digest: 'b'.repeat(64),
    status, revision: status === 'awaiting_review' ? 3 : 1,
    created_at: `2026-11-19T18:00:${String(number).padStart(2, '0')}Z`,
    title: input.prompt ? Array.from(input.prompt.split('\n')[0]).slice(0, 128).join('').replace(/[\r\t]/g, ' ') : 'Fixture existing report',
    prompt: input.prompt ?? 'Review the fixture', acceptance_criteria: input.acceptance_criteria ?? 'Return the complete report',
    auto_repair: input.auto_repair===true, attempt_id: randomUUID(), sequence: 0, events: [], progress_emitted: false,
  };
  if(input.workflow_policy_digest){
    item.workflow_required=true;
    const policy=input.agent_profile==='muse-db-primary'?externalWorkflowPolicy:workflowPolicy;
    item.workflow={kind:input.agent_profile==='muse-db-primary'?'external_database':'native',workflow_id:randomUUID(),policy_digest:policy.policy_digest,phase:'plan_review',author_profile:policy.author_profile,reviewer_profile:policy.reviewer_profile,plan_revisions:0,max_plan_revisions:policy.max_plan_revisions,code_revisions:0,max_code_revisions:policy.max_code_revisions,escalation_reason:null,can_cancel:true};
  }
  event(item, 'queued');
  if (status !== 'queued') {
    event(item, 'started');
    progress(item);
    event(item, status);
  }
  return item;
}
function event(t, kind, report) {
  const item = { event_id:randomUUID(), correlation_id:t.correlation_id, sequence: ++t.sequence, kind, created_at: time };
  if (report?.attempt_id===t.attempt_id) item.progress_verified=true;
  if (report) item.progress = report;
  t.events.push(item);
}
function progress(t) {
  if (t.progress_emitted) return;
  event(t, 'checkpoint', { attempt_id: t.attempt_id, sequence: 1, stage: 'plan', summary: publicPlan });
  event(t, 'checkpoint', { attempt_id: randomUUID(), sequence: 2, stage: 'progress', summary: foreignProgress });
  event(t, 'checkpoint', { attempt_id: t.attempt_id, sequence: 2, stage: 'progress', summary: publicMilestone });
  t.public_summary = publicMilestone;
  t.progress_emitted = true;
}
function taskScope(t) {
  const phase=t.workflow?.phase;
  if(phase==='completed'||t.status==='completed'){
    const d=t.delivery;
    if(d){
      if(['queued','running'].includes(d.verification.status)||['queued','running'].includes(d.repair.status)||['queued','building','deploying'].includes(d.release.status))return 'active';
      if(d.repair.status==='needs_attention'||d.release.status==='failed'||d.verification.status==='failed')return 'attention';
    }
    return 'history';
  }
  if(phase==='cancelled')return 'history';
  if(phase==='escalated')return 'attention';
  if(phase)return 'active';
  if(t.workflow_required)return 'attention';
  if(['queued','running','cancel_requested'].includes(t.status))return 'active';
  return ['completed','cancelled'].includes(t.status)?'history':'attention';
}
function summary(t) {
  return {
    ...Object.fromEntries(['task_id', 'correlation_id', 'project_alias', 'base_revision', 'mode', 'agent_profile', 'recipe_digest', 'status', 'revision', 'created_at', 'title'].map(k => [k, t[k]])),
    workflow_required:t.workflow_required===true,
    workflow_phase:t.workflow?.phase??null,
    ...(t.delivery?{delivery:t.delivery}:{}),
    ...(t.retry_event?{retry_event:t.retry_event}:{}),
  };
}
function detail(t) {
  const processing = ['running', 'cancel_requested'].includes(t.status);
  const fields = summary(t);
  delete fields.workflow_phase;
  delete fields.workflow_enabled;
  delete fields.updated_at;
  if(t.external_result)return {...fields,prompt:t.prompt,acceptance_criteria:t.acceptance_criteria,
    attempt:null,result:null,reconciliation_receipt_id:null,workflow_required:t.workflow_required===true,workflow:t.workflow??null,
    external_result:t.external_result,
    ...(t.correlation_cicd ? { correlation_cicd: t.correlation_cicd } : {})};
  return {
    ...fields, prompt: t.prompt, acceptance_criteria: t.acceptance_criteria,
    attempt: t.status === 'queued' ? null : { attempt_id: t.attempt_id, worker_id: 'fixture-worker', phase: processing ? 'running' : 'stopped', started_at: time, finished_at: processing ? null : time, summary: processing ? t.public_summary ?? 'The fixture agent is working.' : 'Fixture process scope stopped.' },
    result: ['awaiting_review', 'completed'].includes(t.status) ? { attempt_id: t.attempt_id, sha256: digest, byte_count: bytes.length, chunk_count: Math.ceil(bytes.length / 16384) } : null,
    reconciliation_receipt_id: null,
    ...(t.publication ? { publication: t.publication } : {}),
    ...(t.review ? { review:t.review } : {}),
    ...(t.correlation_cicd ? { correlation_cicd: t.correlation_cicd } : {}),
    workflow_required:t.workflow_required===true,workflow:t.workflow??null,
  };
}
function usage(alias, tasks) {
  const p = profiles.find(item => item.alias === alias);
  const active = [...tasks.values()].find(t => t.agent_profile === alias && ['running', 'cancel_requested'].includes(t.status));
  const codex = p.backend === 'codex';
  const grok = p.backend === 'grok';
  return {
    alias, label: p.label, provider: p.backend, model: p.model, worker_state: active ? 'running' : grok ? 'unknown' : 'idle', active_task_id: active?.task_id ?? null,
    snapshot_state: codex ? 'fresh' : 'unknown', observed_at: codex ? time : null, source: 'owned-browser-fixture', version: '1', last_error: null, refresh_pending: false,
    allowance: { state: codex ? 'available' : grok ? 'unknown' : 'unsupported', reason: codex ? null : grok ? 'Not reported' : 'Headless allowance unavailable', ordinary_usage_allowed: codex ? true : null,
      buckets: codex ? [{ key: 'fixture-window', label: 'Provider window', used_percent: 12.5, window_minutes: 90, resets_at: '2026-11-19T19:30:00Z' }] : [] },
    jobs: { scope: 'forge_project', input_tokens: codex ? '9007199254740993' : null, output_tokens: codex ? '5' : null, cached_input_tokens: codex ? '2' : null, total_tokens: codex ? '9007199254740998' : null, known_attempts: codex ? 1 : 0, unknown_attempts: codex || grok ? 0 : 1, elapsed_ms: codex ? '1000' : null },
    account_activity: { state: 'unknown', lifetime_tokens: null, daily: [] },
  };
}

function deliveryProjection(stage) {
  const activeRepair=stage==='repair';
  const deployed=stage==='deployed';
  return {verification_id:'77777777-7777-4777-8777-777777777777',source_kind:'native',
    commit_sha:'f'.repeat(40),tree_sha:'c'.repeat(40),updated_at:time,
    verification:{status:activeRepair?'failed':deployed?'passed':stage==='queued'?'queued':'running',updated_at:time,summary:activeRepair?'Main failed; source-bound repair is queued.':deployed?'All exact main gates passed.':'Main checks are running.',build_number:42},
    repair:{...(activeRepair||deployed?{retry_event:{event_id:'99999999-9999-4999-8999-999999999999',kind:'tests',used:1,limit:20,status:activeRepair?'open':'resolved'}}:{}),status:activeRepair?'queued':deployed?'completed':'none',round:activeRepair||deployed?1:0,max_rounds:20,task_id:activeRepair?'11111111-1111-4111-8111-111111111111':null,updated_at:time,summary:activeRepair?'Independent author and reviewer policy applies.':'No active repair.'},
    release:{status:deployed?'deployed':'none',release_id:deployed?'88888888-8888-4888-8888-888888888888':null,receipt_sha256:deployed?'e'.repeat(64):null,updated_at:time,summary:deployed?'Activation receipt and health confirmed.':'No activation receipt.'}};
}

export async function startAgentsRelay() {
  const directory = await mkdtemp(join(tmpdir(), 'forge-agents-ui-'));
  const tokenFile = join(directory, 'relay-token');
  const token = randomBytes(32).toString('hex');
  await writeFile(tokenFile, token, { mode: 0o600 });
  const seed = task('awaiting_review', 0);
  const tasks = new Map([[seed.task_id, seed]]);
  const receipts = new Map();
  const controls = new Map();
  const calls = [];
  const faults = { dropNextSubmit: false, denyNextSubmit: false, denyNextList: false, denyNextGet: null, holdProgressCompletion: false,
    dropNextPlanDecision: false, holdPublication: false, autonomousPolicy:false,externalWorkflow:false,denyNextEvents:null };
  const planDecisions = [], reviewDecisions = [], publicationRevisions = [], workflowSubmissions=[];
  const verificationRetries = [], reviewReads = [], eventReads = [];
  let refreshes = 0, progressRefetches = 0;
  const server = createServer(async (request, response) => {
    try {
      if (request.method !== 'POST' || request.url !== '/v1/dispatch' || request.headers.authorization !== `Bearer ${token}`) {
        response.writeHead(401, { 'Content-Type': 'application/json' }); response.end('{}'); return;
      }
      const chunks = []; let size = 0;
      for await (const chunk of request) { size += chunk.length; if (size > 16384) throw new Error('fixture body bound'); chunks.push(chunk); }
      const input = JSON.parse(Buffer.concat(chunks));
      const requestId = input.request_id;
      if (request.headers['forge-request-id'] !== requestId || request.headers['forge-envelope-version'] !== '1' || request.headers['forge-contract-version'] !== '1') throw new Error('fixture headers');
      if(input.kind.startsWith('work_') && request.headers['forge-delivery-version']!=='1')throw new Error('fixed native delivery version missing');
      calls.push(input.kind);
      const send = (status, kind, fields = {}) => { response.writeHead(status, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ protocol_version: 1, event: { kind, request_id: requestId, ...fields } })); };
      const failed = (status, code) => send(status, input.kind.startsWith('usage_') ? 'usage_failed' : 'work_failed', { code });
      if (input.kind === 'work_catalog') {
        send(200, 'work_catalog', { projects: [{ alias: project, label: 'Fixture workspace', base_revision: base, ...(faults.autonomousPolicy?{workflow:workflowPolicy}:{}),...(faults.projectDelivery?{delivery:faults.projectDelivery}:{}),...(faults.externalWorkflow?{external_workflow:externalWorkflowPolicy}:{}) }], profiles, worker_status: 'ready', observed_at: time, agent_branches: faults.agentBranches ?? defaultAgentBranches() }); return;
      }
      if (input.project_alias !== project) { failed(403, 'denied'); return; }
      if (input.kind === 'usage_overview' || input.kind === 'usage_detail') {
        const selected = input.kind === 'usage_detail' ? profiles.filter(p => p.alias === input.agent_profile) : profiles;
        send(200, input.kind, { project_alias: project, providers: selected.map(p => usage(p.alias, tasks)) }); return;
      }
      if (input.kind === 'usage_refresh') {
        if (!profiles.some(p => p.alias === input.agent_profile)) { failed(403, 'denied'); return; }
        refreshes++; send(202, 'usage_refresh_queued', { project_alias: project, refresh_id: randomUUID(), profiles: [input.agent_profile], coalesced: false }); return;
      }
      if (input.kind === 'work_list') {
        if(faults.denyNextList){faults.denyNextList=false;failed(503,'unavailable');return;}
        const roots=[...tasks.values()].filter(t=>!t.internal);
        const scoped=['active','attention','history','all'].includes(input.status_filter);
        const counts={active:0,attention:0,history:0,all:roots.length};
        for(const item of roots)counts[taskScope(item)]++;
        const ordered=roots.filter(t=>scoped?(input.status_filter==='all'||taskScope(t)===input.status_filter):(!input.status_filter||t.status===input.status_filter))
          .sort((a,b)=>scoped?(b.updated_at??b.created_at).localeCompare(a.updated_at??a.created_at)||b.task_id.localeCompare(a.task_id):a.created_at.localeCompare(b.created_at)||a.task_id.localeCompare(b.task_id));
        const anchor=input.cursor?ordered.findIndex(t=>t.task_id===input.cursor):-1;
        if(input.cursor&&anchor<0){failed(400,'invalid_request');return;}
        const available=ordered.slice(anchor+1),page=available.slice(0,input.limit??20);
        send(200,'work_listed',{project_alias:project,tasks:page.map(t=>({...summary(t),...(scoped?{updated_at:t.updated_at??t.created_at,...(t.workflow?{workflow_enabled:!['completed','cancelled','escalated'].includes(t.workflow.phase)}:{})}:{})})),next_cursor:available.length>page.length?page.at(-1).task_id:null,...(scoped?{scope_counts:counts}:{})});return;
      }
      if (input.kind === 'work_submit') {
        const external=input.agent_profile==='muse-db-primary';
        if(external && (faults.externalWorkflow?(input.workflow_policy_digest!==externalWorkflowPolicy.policy_digest||input.mode!=='plan'||input.auto_repair===true):(input.workflow_policy_digest!=null||!['plan','review'].includes(input.mode)))){failed(400,'invalid_request');return;}
        if(faults.autonomousPolicy && !external){
          if(input.workflow_policy_digest!==workflowPolicy.policy_digest||input.mode!=='plan'||input.agent_profile!==workflowPolicy.author_profile||input.auto_repair===true){failed(400,'invalid_request');return;}
          workflowSubmissions.push({...input});
        }
        if (faults.denyNextSubmit) { faults.denyNextSubmit = false; failed(403, 'denied'); return; }
        let receipt = receipts.get(input.idempotency_key);
        if (!receipt) {
          const next = task('queued', tasks.size, input); tasks.set(next.task_id, next);
          receipt = { task_id: next.task_id, project_alias: project, revision: 1, status: 'queued', recipe_digest: next.recipe_digest };
          receipts.set(input.idempotency_key, receipt);
          if(external){
            next.status='awaiting_review';next.revision=3;
            const content='# External Muse plan\n\nDatabase handoff fixture.\n<script>globalThis.__externalExecuted=true</script>';
            next.external_result={assignment_id:randomUUID(),provider:'muse-db',sha256:createHash('sha256').update(content).digest('hex'),content,submitted_at:time,summary:'External plan returned'};
          }
        }
        if (faults.dropNextSubmit) { faults.dropNextSubmit = false; response.destroy(); return; }
        send(202, 'work_submitted', receipt); return;
      }
      const selected = tasks.get(input.task_id);
      if (!selected) { failed(404, 'not_found'); return; }
      if (input.kind === 'work_get') {
        if(faults.denyNextGet===selected.task_id){faults.denyNextGet=null;failed(503,'unavailable');return;}
        if (selected.publication && ['pending', 'checking', 'publishing'].includes(selected.publication.status)) {
          publicationRevisions.push(selected.revision);
          if (!faults.holdPublication) {
            selected.publication_step++;
            const stages = ['pending', 'checking', 'publishing', 'published'];
            selected.publication.status = stages[Math.min(selected.publication_step, 3)];
            selected.publication.summary = 'Required fixture checks and PR publication.';
            if (selected.publication.status === 'published') {
              selected.publication.commit_sha = 'e'.repeat(40);
              selected.publication.pr_url = 'https://github.com/roachmaster/homelab/pull/42';
              selected.review.stage='approved';selected.review.required_hosted_green=true;selected.review.can_accept=true;

            }
          }
        }
        if (selected.status === 'queued') { selected.status = 'running'; selected.revision++; event(selected, 'started'); }
        else if (selected.status === 'running' && selected.progress_emitted) progressRefetches++;
        if(selected.workflow && selected.status==='running'){
          progress(selected);selected.status='awaiting_review';selected.revision++;event(selected,'awaiting_review');
        }
        if(selected.review)reviewReads.push({task_id:selected.task_id,review_id:selected.review.review_id,
          revision:selected.revision,source_attempt_id:selected.attempt_id});
        send(200, 'work_opened', { task: detail(selected) }); return;
      }
      if (input.kind === 'work_events') {
        const afterSequence = input.after_sequence ?? 0;
        const denied = faults.denyNextEvents?.task_id === selected.task_id
          && faults.denyNextEvents.after_sequence === afterSequence;
        eventReads.push({ request_id: requestId, task_id: selected.task_id, after_sequence: afterSequence, denied });
        if (denied) { faults.denyNextEvents = null; failed(503, 'unavailable'); return; }
        if (selected.status === 'running' && !selected.progress_emitted) progress(selected);
        else if (selected.status === 'running' && !faults.holdProgressCompletion) {
          selected.status = 'awaiting_review'; selected.revision++; event(selected, 'awaiting_review');
          if (selected.mode === 'implement') {
            selected.review={auto_repair:selected.auto_repair,stage:'awaiting_decision',review_id:randomUUID(),feedback_id:randomUUID(),round:1,max_rounds:3,open_findings:1,verified_findings:0,latest_check_status:'passed',toolchain:'rustc fixture',handoff_reason:null,source_attempt_id:selected.attempt_id,candidate_commit:'e'.repeat(40),workspace_digest:'f'.repeat(64),gate_manifest_sha256:'c'.repeat(64),required_hosted_green:false,can_accept:false,can_repair:true,can_retry:false,
              findings:[{finding_id:'compiler-fixture',category:'source',summary:'Required compiler fixture finding',expected:'Required check passes',actual:'Local rerun passed; owner review required',status:'open'}],
              pipeline_stages:[
                {id:'source-preflight',label:'source and Python preflight',status:'passed'},
                {id:'check',label:'make ci',status:'running'},
                {id:'in-memory-backend',label:'in-memory backend',status:'waiting'},
              ],
              pipeline_availability:'available',
              pipeline_phase:'running'};

          }
        } else if (selected.status === 'cancel_requested') {
          selected.status = 'cancelled'; selected.revision++; event(selected, 'cancelled');
        }
        const events = selected.events.filter(item => item.sequence > (input.after_sequence ?? 0)).slice(0, input.limit ?? 50);
        send(200, 'work_events', { project_alias: project, task_id: selected.task_id, revision: selected.revision, status: selected.status, events, next_sequence: events.at(-1)?.sequence ?? input.after_sequence ?? 0 }); return;
      }
      if(input.kind==='work_review_decide'){
        if(request.headers['forge-destination']!=='ide.work.review.decide')throw new Error('fixture review destination');
        reviewDecisions.push({...input});
        if(input.decision==='retry_infrastructure'){
          const intent=JSON.stringify(Object.fromEntries(Object.entries(input).filter(([key])=>key!=='request_id')));
          let saved=controls.get(input.idempotency_key);
          if(saved&&(saved.kind!=='work_review_decide'||saved.intent!==intent)){failed(409,'idempotency_conflict');return;}
          if(!saved){
            if(input.expected_revision!==selected.revision||input.review_id!==selected.review?.review_id){failed(409,'revision_conflict');return;}
            if(selected.mode!=='implement'||selected.status!=='awaiting_review'||!selected.review.can_retry
                ||!input.note?.trim()||(input.findings??[]).length||(input.verified_finding_ids??[]).length){failed(400,'invalid_request');return;}
            const before=selected.revision,previousReview=selected.review.review_id;
            selected.revision++;
            selected.review={...selected.review,review_id:randomUUID(),stage:'awaiting_decision',
              latest_check_status:'passed',handoff_reason:null,can_retry:false,can_repair:true};
            saved={kind:'work_review_decide',intent,receipt:{project_alias:project,task_id:selected.task_id,
              base_revision:before,revision:selected.revision,status:selected.status,review_id:selected.review.review_id}};
            controls.set(input.idempotency_key,saved);
            verificationRetries.push({request:{...input},previous_review_id:previousReview,receipt:{...saved.receipt},
              source_attempt_id:selected.attempt_id,round:selected.review.round});
          }
          send(202,'work_review_decided',saved.receipt);return;
        }
        if(input.expected_revision!==selected.revision||input.review_id!==selected.review?.review_id){failed(409,'revision_conflict');return;}
        if(input.decision!=='approve'||!input.note?.trim()||JSON.stringify(input.verified_finding_ids)!==JSON.stringify(['compiler-fixture'])){failed(400,'invalid_request');return;}
        const before=selected.revision;selected.revision++;
        selected.review.findings[0].status='verified';selected.review.open_findings=0;selected.review.verified_findings=1;selected.review.stage='waiting_hosted';selected.review.can_repair=false;
        selected.publication={publication_id:randomUUID(),status:'pending',branch:'forge/task-fixture',summary:'Waiting for required hosted checks.'};selected.publication_step=-1;
        send(200,'work_review_decided',{project_alias:project,task_id:selected.task_id,base_revision:before,revision:selected.revision,status:selected.status,review_id:selected.review.review_id});return;
      }
      if (input.kind === 'work_plan_decide') {
        if (request.headers['forge-destination'] !== 'ide.work.plan.decide') throw new Error('fixture plan destination');
        planDecisions.push({ ...input });
        const intent = JSON.stringify(Object.fromEntries(Object.entries(input).filter(([key]) => key !== 'request_id')));
        let saved = controls.get(input.idempotency_key);
        if (saved && (saved.kind !== 'work_plan_decide' || saved.intent !== intent)) { failed(409, 'idempotency_conflict'); return; }
        if (!saved) {
          if (input.expected_revision !== selected.revision) { failed(409, 'revision_conflict'); return; }
          if (selected.mode !== 'plan' || !['awaiting_review', 'completed'].includes(selected.status)
              || input.plan_attempt_id !== selected.attempt_id || input.plan_sha256 !== digest) { failed(409, 'not_reviewable'); return; }
          if (!['resume', 'revise', 'cancel'].includes(input.decision)
              || input.decision === 'revise' && !input.note?.trim()) { failed(400, 'invalid_request'); return; }
          const mode = input.decision === 'resume' ? 'implement' : 'plan';
          if (input.decision !== 'cancel' && !profiles.some(p => p.alias === input.agent_profile && p.modes.includes(mode))) { failed(403, 'denied'); return; }
          const before = selected.revision;
          selected.revision++; selected.mode = mode;
          selected.status = input.decision === 'cancel' ? 'cancelled' : 'queued';
          if (input.decision !== 'cancel') {
            selected.agent_profile = input.agent_profile;
            selected.auto_repair=input.decision==='resume' && input.auto_repair===true;
            selected.review=null;
            selected.attempt_id = randomUUID(); selected.progress_emitted = false;
            selected.publication = null;
            if (input.decision === 'revise') selected.prompt += '\nReview feedback: ' + input.note;
          }
          event(selected, input.decision === 'cancel' ? 'cancelled' : 'resumed');
          saved = { kind: 'work_plan_decide', intent, receipt: { project_alias: project, task_id: selected.task_id,
            base_revision: before, revision: selected.revision, status: selected.status, mode } };
          controls.set(input.idempotency_key, saved);
        }
        if (faults.dropNextPlanDecision) { faults.dropNextPlanDecision = false; response.destroy(); return; }
        send(saved.receipt.status === 'queued' ? 202 : 200, 'work_plan_decided', saved.receipt); return;
      }
      if (input.kind === 'work_cancel' || input.kind === 'work_accept') {
        let receipt = controls.get(input.idempotency_key);
        if (!receipt) {
          if (input.expected_revision !== selected.revision) { failed(409, 'revision_conflict'); return; }
          const before = selected.revision;
          if (input.kind === 'work_accept' && selected.status !== 'awaiting_review') { failed(409, 'not_reviewable'); return; }
          selected.status = input.kind === 'work_accept' ? 'completed' : selected.status === 'queued' ? 'cancelled' : 'cancel_requested'; selected.revision++;
          if(selected.workflow && input.kind==='work_cancel'){selected.workflow.phase='cancelled';selected.workflow.can_cancel=false;selected.status='cancelled';}
          event(selected, selected.status);
          receipt = { project_alias: project, task_id: selected.task_id, base_revision: before, revision: selected.revision, status: selected.status };
          controls.set(input.idempotency_key, receipt);
        }
        send(200, input.kind === 'work_accept' ? 'work_accepted' : 'work_cancel_recorded', receipt); return;
      }
      if (input.kind === 'work_cicd_stage_log') {
        if (input.correlation_id !== selected.correlation_id) { failed(403, 'denied'); return; }
        const logs = selected.cicd_stage_logs ?? {};
        const key = `${input.action_id}:${input.stage_id}:${input.view}`;
        const entry = logs[key];
        if (!entry) { failed(404, 'not_found'); return; }
        const full = Buffer.from(entry.content ?? '', 'utf8');
        const logDigest = createHash('sha256').update(full).digest('hex');
        const [cursorDigest, rawOffset] = input.cursor?.split('.') ?? [logDigest, '0'];
        const offset = Number(rawOffset);
        if (cursorDigest !== logDigest || !Number.isSafeInteger(offset) || offset < 0 || offset > full.length) { failed(409, 'result_mismatch'); return; }
        const limit = Math.min(input.limit_bytes ?? 32768, entry.pageSize ?? 32768);
        let end = Math.min(full.length, offset + limit);
        while (end < full.length && (full[end] & 0xc0) === 0x80) end--;
        const content = full.subarray(offset, end).toString('utf8');
        send(200, 'work_cicd_stage_log_page', {
          project_alias: project,
          task_id: selected.task_id,
          correlation_id: selected.correlation_id,
          action_id: input.action_id,
          stage_id: input.stage_id,
          view: input.view,
          content,
          truncated: end < full.length,
          total_bytes: full.length,
          byte_offset: offset,
          next_cursor: end < full.length ? `${logDigest}.${end}` : null,
          prev_cursor: offset > 0 ? `${logDigest}.${Math.max(0, offset - limit)}` : null,
          tests_passed: entry.tests_passed ?? null,
          tests_failed: entry.tests_failed ?? null,
          tests_skipped: entry.tests_skipped ?? null,
          test_cases: entry.test_cases ?? [],
          error_excerpt: entry.error_excerpt ?? null,
        });
        return;
      }
      if (input.kind === 'work_result') {
        if (!['awaiting_review', 'completed'].includes(selected.status) || input.attempt_id !== selected.attempt_id) { failed(404, 'not_found'); return; }
        const [cursorDigest, rawOffset] = input.cursor?.split('.') ?? [digest, '0'];
        const offset = Number(rawOffset);
        if (cursorDigest !== digest || !Number.isSafeInteger(offset) || offset < 0 || offset > bytes.length) { failed(409, 'result_mismatch'); return; }
        let end = Math.min(bytes.length, offset + (input.limit_bytes ?? 16384));
        while (end < bytes.length && (bytes[end] & 0xc0) === 0x80) end--;
        send(200, 'work_result_page', { project_alias: project, task_id: selected.task_id, attempt_id: selected.attempt_id, result_digest: digest, content: bytes.subarray(offset, end).toString('utf8'), offset_bytes: offset, next_cursor: end === bytes.length ? null : `${digest}.${end}`, complete: end === bytes.length, total_bytes: bytes.length }); return;
      }
      failed(404, 'not_found');
    } catch (_) { if (!response.destroyed) { response.writeHead(503, { 'Content-Type': 'application/json' }); response.end('{}'); } }
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return { endpoint: `127.0.0.1:${server.address().port}`, tokenFile, faults, calls,
    planDecisions, reviewDecisions, publicationRevisions,workflowSubmissions,verificationRetries,reviewReads,eventReads,
    // This corpus is created directly in owned fixture memory. Its journey may
    // read and navigate; it never submits/retries/cancels/accepts an assignment.
    seedTriage(){
      const add=(prompt,phase,{external=false,status='awaiting_review',retry=null,revisionCount=0}={})=>{
        const next=task(status,tasks.size,{prompt,mode:'plan',agent_profile:external?'muse-db-primary':'agy-primary',workflow_policy_digest:external?externalWorkflowPolicy.policy_digest:workflowPolicy.policy_digest});
        next.updated_at=`2026-11-23T12:00:${String(tasks.size).padStart(2,'0')}Z`;
        next.workflow.phase=phase;next.workflow.plan_revisions=revisionCount;
        next.workflow.escalation_reason=phase==='escalated'?'Independent review requires operator attention; the saved plan is preserved.':null;
        if(retry){next.retry_event={event_id:randomUUID(),kind:'planning',used:retry.used,limit:20,status:retry.status};next.workflow.retry_event={...next.retry_event};}
        if(external){const content='# Owned Muse triage plan\n\nReview the preserved database artifact.';next.external_result={assignment_id:randomUUID(),provider:'muse-db',sha256:createHash('sha256').update(content).digest('hex'),content,submitted_at:time,summary:'Owned external triage fixture'};}
        tasks.set(next.task_id,next);return next;
      };
      const rows=Array.from({length:12},(_,index)=>add(`Review bounded fixture component ${index+1} and preserve its existing approval gates.`,index%2?'code_review':'checks'));
      const prompt='Make the authoritative assignment state understandable while preserving independent reviews and verification across extraordinarilylongword boundaries.\nFull assignment evidence remains here, including this second line.';
      const automatic=add(prompt,'plan_revision',{retry:{used:1,status:'open'},revisionCount:7});
      const boundary=add('The last admitted automatic retry is still Active until its workflow changes.','plan_revision',{retry:{used:20,status:'open'},revisionCount:44});
      const escalation=add('Investigate the preserved author after independent review requires attention.','escalated',{status:'cancelled',retry:{used:20,status:'exhausted'},revisionCount:29});
      const earlyEscalation=add('Workflow escalation requires attention even before its retry budget is spent.','escalated',{status:'cancelled',retry:{used:1,status:'open'},revisionCount:1});
      const muse=add('Review the external Muse assignment before its independent reviewer advances it.','plan_review',{external:true,revisionCount:3});
      const delivery=add('Keep worker readiness separate from exact main verification.','completed',{status:'completed'});
      delivery.workflow.can_cancel=false;delivery.workflow.merge_commit='f'.repeat(40);delivery.delivery=deliveryProjection('queued');
      const manual=task('awaiting_review',tasks.size,{mode:'plan',prompt:'Review this saved plan before authorizing implementation.'});tasks.set(manual.task_id,manual);
      return {rows:rows.map(row=>row.task_id),automatic:automatic.task_id,boundary:boundary.task_id,escalation:escalation.task_id,earlyEscalation:earlyEscalation.task_id,muse:muse.task_id,delivery:delivery.task_id,manual:manual.task_id,prompt};
    },
    // Same-row content change proves background patching without new task identity.
    updateTriagePhase(taskId,phase){const selected=tasks.get(taskId);if(!selected?.workflow)throw new Error('No fixture workflow');selected.workflow.phase=phase;selected.revision++;},
    requireVerificationRetry(taskId){
      const selected=tasks.get(taskId);
      if(selected?.mode!=='implement'||selected.status!=='awaiting_review'||!selected.review)throw new Error('No finished fixture implementation');
      selected.review={...selected.review,stage:'handoff',latest_check_status:'unavailable',
        handoff_reason:'Controlled verification infrastructure failure; author source is preserved.',can_retry:true,can_repair:false};
      selected.revision++;
      return JSON.parse(JSON.stringify(detail(selected)));
    },
    reviewSnapshot(taskId){const selected=tasks.get(taskId);if(!selected)throw new Error('No fixture task');return JSON.parse(JSON.stringify(detail(selected)));},
    enableAutonomy(){faults.autonomousPolicy=true;},
    enableExternal(){if(profiles.some(p=>p.alias==='muse-db-primary'))return;profiles.push({alias:'muse-db-primary',label:'Muse database runner',backend:'muse',model:'External runner',modes:['plan','review'],branch:'forge/agent/muse-db-primary',transport:'external_db'});},
    setCorrelationCicd(taskId,cicd,stageLogs={}){
      const selected=tasks.get(taskId);if(!selected)throw new Error('No fixture task');
      selected.correlation_cicd={...cicd,correlation_id:selected.correlation_id};
      selected.cicd_stage_logs=stageLogs;
      selected.revision++;
    },
    enableExternalWorkflow(){faults.externalWorkflow=true;profiles.find(p=>p.alias==='muse-db-primary').modes=['plan','review','implement'];},
    addHistory(){const next=task('awaiting_review',tasks.size,{prompt:'Complete event history fixture'});next.events=[];next.sequence=0;for(let i=1;i<=61;i++)event(next,'checkpoint',{attempt_id:next.attempt_id,sequence:Math.min(i,128),stage:i===1?'plan':'progress',summary:`History checkpoint ${i}. Literal <img src=x onerror=globalThis.__historyExecuted=true> remains text.`});tasks.set(next.task_id,next);return next.task_id;},
    get latestWorkflowTaskId(){return [...tasks.values()].filter(t=>t.workflow).at(-1)?.task_id;},
    setDelivery(taskId,stage){const selected=tasks.get(taskId);if(!selected)throw new Error('No fixture task');selected.delivery=deliveryProjection(stage);},
    setPipeline(taskId,stages,availability,phase){const selected=tasks.get(taskId);if(!selected?.review)throw new Error('No fixture review');selected.review.pipeline_stages=stages;selected.review.pipeline_availability=availability;selected.review.pipeline_phase=phase;selected.revision++;},
    setProjectDelivery(stage){faults.projectDelivery=deliveryProjection(stage);faults.projectDelivery.source_kind='observed';},
    retryEvent(taskId,kind,used,{status='open',total=used}={}){
      const selected=tasks.get(taskId);if(!selected?.workflow)throw new Error('No fixture workflow');
      selected.retry_events??={};const event_id=selected.retry_events[kind]??=randomUUID();
      selected.retry_event={event_id,kind,used,limit:20,status};selected.workflow.retry_event={...selected.retry_event};
      selected.workflow.code_revisions=total;selected.revision++;
    },
    workflowPhase(taskId,phase,{authorStopped=false}={}){
      const selected=tasks.get(taskId);if(!selected?.workflow)throw new Error('No fixture workflow');
      if(authorStopped){
        if(phase!=='escalated')throw new Error('Only escalation can stop a fixture author');
        selected.status='cancelled';event(selected,'cancelled');
      }
      selected.workflow.phase=phase;selected.workflow.can_cancel=!['completed','cancelled','merging'].includes(phase);
      selected.workflow.plan_revisions=1;
      selected.workflow.reviewer_task_id=null;
      if(selected.workflow.kind==='external_database' && ['plan_review','code_review'].includes(phase)){
        const reviewer=task('awaiting_review',tasks.size,{mode:'review',agent_profile:'codex-primary',prompt:'Independent external workflow review'});reviewer.internal=true;reviewer.workflow_required=true;reviewer.workflow={...selected.workflow,reviewer_task_id:reviewer.task_id};tasks.set(reviewer.task_id,reviewer);selected.workflow.reviewer_task_id=reviewer.task_id;
      }
      const record={event_id:randomUUID(),correlation_id:selected.correlation_id,sequence:++selected.sequence,kind:'checkpoint',created_at:time,workflow_progress:{workflow_id:selected.workflow.workflow_id,phase,summary:`Trusted fixture workflow: ${phase}.`}};
      if(selected.workflow.reviewer_task_id)record.workflow_progress.related_task_id=selected.workflow.reviewer_task_id;
      selected.events.push(record);selected.revision++;
      if(['checks','code_review','merging','completed'].includes(phase))selected.workflow.candidate_commit='e'.repeat(40);
      if(phase==='completed')selected.workflow.merge_commit='f'.repeat(40);
      selected.workflow.escalation_reason=phase==='escalated'?'Revision budget exhausted; artifacts preserved.':null;
      if(phase==='completed')selected.status='completed';
    },
    addPlan(status, title) {
      if (!['awaiting_review', 'completed'].includes(status)) throw new Error('fixture plan state');
      const next = task(status, tasks.size, { mode: 'plan', prompt: title }); tasks.set(next.task_id, next); return next.task_id;
    },
    get taskCount() { return tasks.size; }, get refreshes() { return refreshes; },
    get progressRefetches() { return progressRefetches; },
    async close() { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); await rm(directory, { recursive: true, force: true }); },
  };
}

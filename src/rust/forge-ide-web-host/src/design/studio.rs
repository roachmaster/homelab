use super::{markup, text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[element!(Article [Class = "studio"];
    element!(H1; text("Forge Studio")),
    element!(P [Id = "studio-status", Role = "status"]; text("Connecting to Studio…")),
    element!(Form [Id = "studio-controls", Hidden = ""];
        element!(Label;
            text("Project"),
            element!(Select [Id = "studio-project", Required = ""]),
        ),
        element!(Label;
            text("Build profile"),
            element!(Select [Id = "studio-profile", Required = ""]),
        ),
        element!(Label;
            text("Commit revision"),
            element!(Input [Id = "studio-revision", Type = "text", Autocomplete = "off", Maxlength = "40", Required = ""]),
        ),
        element!(Div [Class = "studio-actions"];
            element!(Button [Id = "studio-submit", Type = "submit"]; text("Build")),
            element!(Button [Id = "studio-cancel", Type = "button", Hidden = ""]; text("Cancel build")),
            element!(Button [Id = "studio-preview", Type = "button", Hidden = ""]; text("Open preview")),
        ),
    ),
    element!(Ol [Id = "studio-events", AriaLabel = "Build progress", Hidden = ""]),
)];

pub fn stylesheet() -> String {
    markup::stylesheet(&[
        (
            ".studio",
            &[("max-inline-size", "40rem"), ("padding-block", "2rem")],
        ),
        ("#studio-controls", &[("max-inline-size", "32rem")]),
        (
            "#studio-controls label",
            &[
                ("display", "grid"),
                ("gap", ".35rem"),
                ("margin-block-end", "1rem"),
            ],
        ),
        (
            "#studio-controls input, #studio-controls select, .studio-actions button",
            &[
                ("font", "inherit"),
                ("min-block-size", "2.5rem"),
                ("max-inline-size", "100%"),
            ],
        ),
        (
            ".studio-actions",
            &[("display", "flex"), ("flex-wrap", "wrap"), ("gap", ".6rem")],
        ),
    ])
}

pub const WORKER: &str = r#"'use strict';
const routes = new Map([
  ['resume', 'ui.session.resume'],
  ['game_project_list', 'game.project.list'],
  ['game_build_submit', 'game.build.submit'],
  ['game_build_get', 'game.build.get'],
  ['game_build_cancel', 'game.build.cancel'],
  ['game_artifact_get', 'game.artifact.get'],
]);
let csrf = '';
self.onmessage = async ({data: {sequence, kind, input}}) => {
  const route = routes.get(kind);
  if (!route) return;
  try {
    const requestId = crypto.randomUUID();
    const headers = {
      'Content-Type': 'application/json', 'Forge-Destination': route,
      'Forge-Envelope-Version': '1', 'Forge-Contract-Version': '1', 'Forge-Request-Id': requestId,
    };
    if (csrf) headers['Forge-CSRF'] = csrf;
    const response = await fetch('/forge/v1/dispatch', {
      method: 'POST', headers, credentials: 'same-origin', redirect: 'error',
      body: JSON.stringify({...input, kind, request_id: requestId}),
    });
    const body = await response.json();
    if (kind === 'resume') {
      if (!response.ok || body.protocol_version !== 1 || body.event?.request_id !== requestId ||
          body.event?.kind !== 'paired' || typeof body.csrf !== 'string') {
        throw new Error('Sign in to Forge IDE, then reopen Studio.');
      }
      csrf = body.csrf;
    } else if (!response.ok || body.request_id !== requestId) {
      const messages = {not_found: 'That project, build, or preview is unavailable.',
        idempotency_conflict: 'This build request conflicts with a previous submission.',
        capacity: 'Studio is busy. Try again shortly.'};
      throw new Error(messages[body.code] || 'Studio could not complete the request.');
    }
    self.postMessage({sequence, body: kind === 'resume' ? {} : body});
  } catch (error) {
    self.postMessage({sequence, failure: error.message});
  }
};
"#;

pub const MAIN: &str = r#"'use strict';
const form = document.querySelector('#studio-controls');
const status = document.querySelector('#studio-status');
const project = document.querySelector('#studio-project');
const profile = document.querySelector('#studio-profile');
const revision = document.querySelector('#studio-revision');
const submit = document.querySelector('#studio-submit');
const cancel = document.querySelector('#studio-cancel');
const preview = document.querySelector('#studio-preview');
const events = document.querySelector('#studio-events');
const storageKey = 'forge.studio.workspace';
const worker = new Worker('/studio/worker.js');
const pending = new Map();
const states = {queued: 'Queued', running: 'Building', completed: 'Ready', succeeded: 'Ready',
  failed: 'Failed', cancelled: 'Cancelled', retrying: 'Waiting to retry'};
let sequence = 0;
let projects = [];
let build = null;
let intent = null;
let timer = 0;
let busy = false;
let buildRevision = 0;

worker.onmessage = ({data}) => {
  const handlers = pending.get(data.sequence);
  if (!handlers) return;
  pending.delete(data.sequence);
  if (data.failure) handlers.reject(new Error(data.failure));
  else handlers.resolve(data.body);
};
worker.onerror = () => {
  for (const {reject} of pending.values()) reject(new Error('Studio is unavailable.'));
  pending.clear();
};

function dispatch(kind, input = {}) {
  const serial = ++sequence;
  return new Promise((resolve, reject) => {
    pending.set(serial, {resolve, reject});
    worker.postMessage({sequence: serial, kind, input});
  });
}

function selected() {
  return projects[Number(project.value)];
}

function projectKey(item) {
  return {node: item.node, project: item.project};
}

function store() {
  try {
    sessionStorage.setItem(storageKey, JSON.stringify({
      project: projectKey(selected()), profile: profile.value, revision: revision.value,
      build_id: build?.build_id, intent,
    }));
  } catch (_) {}
}

function load() {
  try { return JSON.parse(sessionStorage.getItem(storageKey)); } catch (_) { return null; }
}

function active() {
  return build !== null && ['queued', 'running', 'retrying'].includes(build.state);
}

function refreshControls() {
  submit.disabled = busy || active();
  for (const control of [project, profile, revision]) control.disabled = busy || active();
  cancel.hidden = !active();
  cancel.disabled = busy;
  preview.hidden = !build?.artifact;
  preview.disabled = busy;
}

function renderBuild(value) {
  build = value;
  buildRevision++;
  status.textContent = states[value.state] || value.state;
  events.replaceChildren(...value.events.map(event => {
    const item = document.createElement('li');
    item.textContent = states[event.state] || event.state;
    return item;
  }));
  events.hidden = value.events.length === 0;
  refreshControls();
  store();
  clearTimeout(timer);
  if (active()) timer = setTimeout(poll, 1500);
}

async function poll() {
  const id = build?.build_id;
  if (!id) return;
  const requested = buildRevision;
  if (busy) {
    timer = setTimeout(poll, 1500);
    return;
  }
  try {
    const reply = await dispatch('game_build_get', {project: projectKey(selected()), build_id: id});
    if (requested === buildRevision) renderBuild(reply.build);
  } catch (error) {
    if (requested !== buildRevision) return;
    status.textContent = error.message;
    timer = setTimeout(poll, 3000);
  }
}

function setProject() {
  const item = selected();
  profile.replaceChildren(...item.profiles.map(name => new Option(name, name)));
  revision.value = item.revision;
}

async function connect() {
  await dispatch('resume');
  const reply = await dispatch('game_project_list');
  projects = reply.projects;
  if (projects.length === 0) {
    status.textContent = 'No Studio projects are available for this session.';
    return;
  }
  project.replaceChildren(...projects.map((item, index) => new Option(item.project + ' · ' + item.node, index)));
  const saved = load();
  const index = projects.findIndex(item => item.node === saved?.project?.node && item.project === saved.project.project);
  if (index >= 0) project.selectedIndex = index;
  setProject();
  form.hidden = false;
  status.textContent = '';
  if (index < 0) return;
  if (selected().profiles.includes(saved.profile)) profile.value = saved.profile;
  if (/^[a-f0-9]{40}$/.test(saved.revision)) revision.value = saved.revision;
  if (saved.intent && typeof saved.intent.key === 'string') intent = saved.intent;
  if (typeof saved.build_id === 'string') {
    const reply = await dispatch('game_build_get', {project: projectKey(selected()), build_id: saved.build_id});
    renderBuild(reply.build);
  }
}

project.addEventListener('change', () => {
  clearTimeout(timer);
  intent = null;
  build = null;
  buildRevision++;
  events.hidden = true;
  status.textContent = '';
  setProject();
  refreshControls();
  store();
});

form.addEventListener('submit', async event => {
  event.preventDefault();
  if (busy || active()) return;
  if (!/^[a-f0-9]{40}$/.test(revision.value)) {
    status.textContent = 'Enter the full 40-character commit revision.';
    return;
  }
  const input = {project: projectKey(selected()), profile: profile.value, revision: revision.value};
  const target = JSON.stringify(input);
  if (intent?.target !== target) intent = {target, key: crypto.randomUUID()};
  busy = true;
  store();
  refreshControls();
  try {
    const reply = await dispatch('game_build_submit', {...input, idempotency_key: intent.key});
    intent = null;
    renderBuild(reply.build);
  } catch (error) {
    status.textContent = error.message;
  } finally {
    busy = false;
    refreshControls();
  }
});

cancel.addEventListener('click', async () => {
  if (busy || !active()) return;
  busy = true;
  buildRevision++;
  clearTimeout(timer);
  refreshControls();
  try {
    const reply = await dispatch('game_build_cancel', {project: projectKey(selected()), build_id: build.build_id});
    renderBuild(reply.build);
  } catch (error) {
    status.textContent = error.message;
    if (active()) timer = setTimeout(poll, 3000);
  } finally {
    busy = false;
    refreshControls();
  }
});

preview.addEventListener('click', async () => {
  if (busy || !build?.artifact) return;
  busy = true;
  refreshControls();
  try {
    const reply = await dispatch('game_artifact_get', {project: projectKey(selected()), build_id: build.build_id});
    store();
    location.assign(reply.artifact.preview_url);
  } catch (error) {
    status.textContent = error.message;
  } finally {
    busy = false;
    refreshControls();
  }
});

connect().catch(error => { status.textContent = error.message; });
"#;

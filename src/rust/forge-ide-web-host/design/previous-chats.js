'use strict';

const previousChats = document.getElementById('previous-chats');
const menu = document.getElementById('previous-chat-menu');
const trigger = menu.querySelector('summary');
const title = document.getElementById('previous-chat-title');
const transcript = document.getElementById('previous-chat-transcript');
const compose = document.getElementById('previous-chat-compose');
const draft = document.getElementById('previous-chat-draft');
const archive = document.getElementById('previous-chat-archive');
const status = document.getElementById('previous-chat-status');
const local = document.getElementById('previous-chat-local');
const localRows = document.getElementById('previous-chat-local-rows');

// The preview uses fictional messages and keeps all edits in this document.
const chats = new Map([
  ['workbench', {
    title: 'Summarize Workbench',
    messages: [
      ['user', 'Summarize Workbench.'],
      ['assistant', 'Use Explorer to find a file, File to read its source, and Outline to jump to a declaration.'],
    ],
  }],
  ['source', {
    title: 'Explain source verification',
    messages: [
      ['user', 'How do I know the file matches the inspection?'],
      ['assistant', 'Forge compares the bytes it reads with the digest from the inspection. If the file changed, refresh the project.'],
    ],
  }],
  ['backup', {
    title: 'Plan a backup',
    messages: [
      ['user', 'What should I check when backing up PostgreSQL?'],
      ['assistant', 'Restore the backup into a disposable database and verify that the expected schema and migration are present.'],
    ],
  }],
  ['archive', {
    title: 'Inspect a Rust project',
    archived: true,
    messages: [
      ['user', 'Where should I start with a Rust project?'],
      ['assistant', 'Read the project README, open Cargo.toml, and look for the library or binary entry point.'],
    ],
  }],
]);
const drafts = new Map([['workbench', draft.value]]);
let selected = 'workbench';
let nextLocal = 0;

function presentChat(id) {
  const chat = chats.get(id);
  selected = id;
  title.textContent = chat.title;
  const messages = chat.messages.map(([role, text]) => {
    const message = document.createElement('p');
    message.dataset.role = role;
    const speaker = document.createElement('strong');
    speaker.textContent = role === 'user' ? 'You' : 'Forge';
    const body = document.createElement('span');
    body.textContent = text;
    message.append(speaker, body);
    return message;
  });
  if (messages.length === 0) {
    const empty = document.createElement('p');
    empty.textContent = 'Start a conversation.';
    messages.push(empty);
  }
  transcript.replaceChildren(...messages);
  compose.hidden = Boolean(chat.archived);
  archive.hidden = !chat.archived;
  draft.value = drafts.get(id) ?? '';
  for (const button of menu.querySelectorAll('[data-preview-chat]')) {
    if (button.dataset.previewChat === id) button.setAttribute('aria-current', 'true');
    else button.removeAttribute('aria-current');
  }
}

function openChat(id) {
  presentChat(id);
  menu.open = false;
  status.textContent = chats.get(id).archived
    ? 'Archived snapshot opened.'
    : 'Opened ' + chats.get(id).title + '.';
  trigger.focus();
}

function newChat(from) {
  const id = 'local-' + ++nextLocal;
  const chat = {
    title: from ? from.title + ' (copy)' : nextLocal === 1 ? 'New chat' : 'New chat ' + nextLocal,
    messages: from?.messages ?? [],
  };
  chats.set(id, chat);
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'previous-chat-row';
  button.dataset.previewChat = id;
  button.textContent = chat.title;
  localRows.prepend(button);
  local.hidden = false;
  openChat(id);
  draft.focus();
}

menu.addEventListener('click', event => {
  const button = event.target.closest('[data-preview-chat]');
  const id = button?.dataset.previewChat;
  if (chats.has(id)) openChat(id);
});
menu.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || !menu.open) return;
  event.preventDefault();
  menu.open = false;
  trigger.focus();
});
previousChats.addEventListener('pointerdown', event => {
  if (menu.open && !menu.contains(event.target)) menu.open = false;
});
draft.addEventListener('input', () => drafts.set(selected, draft.value));
previousChats.querySelector('#previous-chat-new').addEventListener('click', () => newChat());
previousChats.querySelector('#previous-chat-continue').addEventListener('click', () => {
  const chat = chats.get(selected);
  if (chat.archived) newChat(chat);
});
presentChat(selected);

// Lambda Memory local web UI. One ES module, no dependencies. The DOM is built only with
// createElement/textContent; every API call carries the per-start CSRF token; nothing is retried
// automatically (a 502 means the write was not confirmed and must be checked by the user).

const CSRF = document.querySelector('meta[name="lm-csrf"]')?.content ?? '';
const STATES = ['active', 'processing', 'conflicted', 'rejected', 'superseded'];
const TIMELINE_STATUSES = ['proposed', 'adopted', 'superseded', 'reverted', 'retracted'];
const OPERATIONS = ['capture', 'propose', 'dream', 'list', 'recall', 'review', 'purge', 'timeline'];
const OUTCOMES = ['running', 'succeeded', 'failed', 'indeterminate'];
const TAG_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const POLL_MS = 10000;
const MAX_PAGES = 20; // detail views page through history up to 20 × 32 items
const NOT_CONFIRMED = 'Not confirmed — check before retrying.';

const prefs = { memoriesTab: 'active', timelineOrder: 'asc', timelineStatuses: new Set(), uncoveredOpen: false };

// Preserve the installed preference; Auto follows the operating system.
const THEMES = [['auto', 'Auto'], ['light', 'Light'], ['dark', 'Dark']];
function readTheme() { try { const value = localStorage.getItem('lm-theme'); return value === 'light' || value === 'dark' ? value : 'auto'; } catch { return 'auto'; } }
let theme = readTheme();
function applyTheme(value) {
  theme = value;
  if (value === 'auto') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = value;
  try { if (value === 'auto') localStorage.removeItem('lm-theme'); else localStorage.setItem('lm-theme', value); } catch { /* Applies for this page even when storage is unavailable. */ }
}
applyTheme(theme);

const STATE_MEANINGS = {
  active: 'Available for recall. Historical evidence, not verified truth.',
  processing: 'Automatic preparation is unfinished; this is not an approval queue.',
  conflicted: 'Conflicting revisions need resolution before they can be recalled.',
  rejected: 'Retired from review and excluded from recall; source evidence is retained.',
  superseded: 'Replaced by a newer active revision; retained as history.',
};
const TIMELINE_MEANINGS = {
  proposed: 'A proposed decision, not yet adopted.',
  adopted: 'Recorded as an adopted project decision, not independently verified truth.',
  superseded: 'Recorded as replaced by another decision.',
  reverted: 'Recorded as a decision that was reversed.',
  retracted: 'Withdrawn from the curated account; history and source coverage remain.',
};
let generation = 0;
let cleanups = [];

// ---------------------------------------------------------------------------------------------
// DOM helpers

const PROPS = new Set(['value', 'checked', 'disabled', 'selected', 'hidden', 'required', 'maxLength', 'multiple', 'open']);

function h(tag, props, ...children) {
  const el = document.createElement(tag);
  if (props) {
    for (const [key, value] of Object.entries(props)) {
      if (value === undefined || value === null || value === false) continue;
      if (key === 'text') el.textContent = String(value);
      else if (key === 'class') el.className = value;
      else if (key.startsWith('on') && typeof value === 'function') el.addEventListener(key.slice(2), value);
      else if (PROPS.has(key)) el[key] = value;
      else if (key === 'href' && !String(value).startsWith('#')) throw new Error('only in-app links');
      else el.setAttribute(key, value === true ? '' : String(value));
    }
  }
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : String(child));
  }
  return el;
}
const mono = (value) => h('code', { class: 'mono', text: value ?? '—' });
const badge = (value, extra = '') => h('span', { class: `badge ${value ?? ''} ${extra}`.trim(), text: value ?? 'unknown' });
const fmtTime = (ms) => (ms === null || ms === undefined ? '—' : new Date(ms).toLocaleString());
function timeEl(ms) {
  if (ms === null || ms === undefined) return h('span', { text: '—' });
  const date = new Date(ms);
  if (!Number.isFinite(date.getTime())) return h('span', { text: String(ms) });
  return h('time', { datetime: date.toISOString(), text: date.toLocaleString() });
}
const fmtDuration = (ms) => (ms === null || ms === undefined ? '—' : ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`);
const bytes = (text) => new TextEncoder().encode(text).length;
const enc = encodeURIComponent;
const link = (project, section, id) => `#/p/${enc(project)}/${section}${id === undefined ? '' : `/${enc(id)}`}`;

function kv(pairs) {
  return h('dl', { class: 'kv' }, pairs.filter(Boolean).flatMap(([key, value]) => [h('dt', { text: key }), h('dd', null, value ?? '—')]));
}
function notice(kind, title, detail) {
  return h('div', { class: `notice ${kind}`, role: kind === 'error' ? 'alert' : 'status' },
    h('strong', { text: title }), detail ? h('div', { class: 'content', text: detail }) : null);
}
function card(...children) { return h('section', { class: 'card' }, ...children); }
function disclosure(label, ...children) {
  return h('details', { class: 'disclosure' }, h('summary', { text: label }), h('div', { class: 'disclosure-body' }, ...children));
}
function stateMeaning(state, timeline = false) {
  return h('p', { class: 'state-explanation', text: (timeline ? TIMELINE_MEANINGS : STATE_MEANINGS)[state] ?? 'State reported by the project worker.' });
}
function projectLabel(project) { return project.split('/').filter(Boolean).pop() || project; }
function themeSwitch() {
  const group = h('div', { class: 'theme-controls', role: 'group', 'aria-label': 'Theme' });
  for (const [value, label] of THEMES) group.append(h('button', {
    type: 'button', text: label, 'aria-pressed': String(theme === value),
    onclick: (event) => {
      applyTheme(value);
      for (const button of group.children) button.setAttribute('aria-pressed', String(button === event.currentTarget));
    },
  }));
  return group;
}
function onCleanup(fn) { cleanups.push(fn); }

let toastBox;
function toast(kind, text) {
  if (!toastBox?.isConnected) { toastBox = h('div', { class: 'toasts', 'aria-live': 'polite' }); document.body.append(toastBox); }
  const close = h('button', { class: 'small', type: 'button', text: 'Dismiss' });
  const item = h('div', { class: `notice ${kind}`, role: kind === 'error' ? 'alert' : 'status' }, h('div', { class: 'content', text }), close);
  close.addEventListener('click', () => item.remove());
  toastBox.append(item);
  if (kind === 'ok') setTimeout(() => item.remove(), 6000);
}

// ---------------------------------------------------------------------------------------------
// API

class ApiError extends Error {
  constructor(kind, message, status, body) { super(message); this.kind = kind; this.status = status; this.body = body; }
}
function errorText(body) {
  if (typeof body === 'string') return body || 'The tool returned an error.';
  if (body && typeof body.error === 'string') {
    const warning = body.diagnostics_warning ? ` (diagnostics: ${body.diagnostics_warning.code})` : '';
    return body.error + warning;
  }
  return JSON.stringify(body);
}

/** Calls the daemon. Throws ApiError; never retries. */
async function api(path, body) {
  const init = {
    method: body === undefined ? 'GET' : 'POST',
    headers: { 'X-Lambda-Memory-CSRF': CSRF, 'Content-Type': 'application/json' },
    credentials: 'same-origin',
    cache: 'no-store',
  };
  if (body !== undefined) init.body = JSON.stringify(body);
  let response;
  try { response = await fetch(path, init); } catch {
    throw new ApiError('unconfirmed', `The daemon did not answer. ${NOT_CONFIRMED}`, 0, null);
  }
  let data = null;
  try { data = await response.json(); } catch { data = null; }
  if (response.ok) return data;
  const status = response.status;
  if (status === 409 && data?.needs_start) throw new ApiError('needs_start', 'The worker for this project is not running.', status, data);
  if (status === 502 && data?.capture_saved) throw new ApiError('unconfirmed', 'Evidence was saved but the correction was not confirmed; check before retrying.', status, data);
  if (status === 502) throw new ApiError('unconfirmed', NOT_CONFIRMED, status, data);
  if (status === 503 && data?.retry) throw new ApiError('busy', `${data.message ?? 'Memory service busy.'} You may retry.`, status, data);
  if (status === 400) throw new ApiError('invalid', `Invalid request: ${data?.error ?? 'the daemon rejected the arguments'}. Nothing was written.`, status, data);
  if (status === 429) throw new ApiError('http', 'Too many active projects in the UI (at most 4). Wait for an idle project lease to expire.', status, data);
  if (status === 403) throw new ApiError('http', 'Forbidden. Reload the page if the daemon restarted (the CSRF token changes on each start).', status, data);
  throw new ApiError('http', data?.error ?? data?.message ?? `Request failed (HTTP ${status}).`, status, data);
}
async function tool(project, name, args = {}) {
  const result = await api('/api/tool', { project, name, arguments: args });
  if (result?.isError) throw new ApiError('tool', errorText(result.body), 200, result.body);
  return result.body;
}
function describe(error) {
  if (error instanceof ApiError) return error.message;
  return error?.message ?? String(error);
}
function failure(error) {
  const title = error?.kind === 'unconfirmed' ? 'Not confirmed' : error?.kind === 'tool' ? 'Tool error' : error?.kind === 'invalid' ? 'Check the form' : 'Request failed';
  return notice('error', title, describe(error));
}
/** Reports an action failure; a stopped worker sends the user back to the start panel. */
function reportError(error) {
  toast('error', describe(error));
  if (error?.kind === 'needs_start') render();
}
/** Runs a user-initiated action once, with its button disabled while in flight. */
async function act(button, work, success) {
  button.disabled = true;
  try {
    const result = await work();
    if (success) toast('ok', success(result));
    return result;
  } catch (error) {
    reportError(error);
    return undefined;
  } finally {
    if (button.isConnected) button.disabled = false;
  }
}

// ---------------------------------------------------------------------------------------------
// Dialogs

/**
 * A modal with a confirm button that stays open on failure and shows the error inline.
 * `typed`: the exact text the user must type before confirming. `run` returns a success message.
 */
function actionDialog({ title, body = [], confirmLabel, danger = false, typed, run }) {
  const errorBox = h('div');
  const input = typed === undefined ? null : h('input', { type: 'text', autocomplete: 'off', spellcheck: 'false', class: 'mono' });
  const confirm = h('button', { type: 'submit', class: danger ? 'danger' : 'primary', text: confirmLabel, disabled: input !== null });
  const cancel = h('button', { type: 'button', text: 'Cancel' });
  const form = h('form', null,
    h('h2', { text: title }), body,
    input ? h('label', null, h('span', null, 'Type ', mono(typed), ' to confirm'), input) : null,
    errorBox, h('div', { class: 'actions' }, cancel, confirm));
  const dialog = h('dialog', { 'aria-label': title }, form);
  let busy = false;
  input?.addEventListener('input', () => { confirm.disabled = input.value !== typed; });
  cancel.addEventListener('click', () => { if (!busy) dialog.close(); });
  dialog.addEventListener('cancel', (event) => { if (busy) event.preventDefault(); });
  dialog.addEventListener('close', () => dialog.remove());
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    if (busy || (input && input.value !== typed)) return;
    busy = true; confirm.disabled = true; cancel.disabled = true; errorBox.replaceChildren();
    try {
      const message = await run(input?.value);
      busy = false; dialog.close();
      if (message) toast('ok', message);
    } catch (error) {
      busy = false; cancel.disabled = false; confirm.disabled = input ? input.value !== typed : false;
      errorBox.replaceChildren(failure(error));
      if (error?.kind === 'needs_start') { dialog.close(); render(); }
    }
  });
  document.body.append(dialog);
  dialog.showModal();
  (input ?? confirm).focus();
  return dialog;
}

// ---------------------------------------------------------------------------------------------
// Routing and shell

function decode(part) { try { return decodeURIComponent(part); } catch { return null; } }
function route() {
  const parts = location.hash.replace(/^#\/?/, '').split('/').filter((part) => part !== '');
  if (parts[0] !== 'p' || parts.length < 2) return { view: 'projects' };
  const project = decode(parts[1]);
  if (!project) return { view: 'projects' };
  const section = parts[2] ?? 'dashboard';
  const id = parts[3] === undefined ? undefined : decode(parts[3]);
  if (section === 'memories') return { view: 'memories', project, nav: 'memories' };
  if (section === 'memory' && id) return { view: 'memory', project, id, nav: 'memories' };
  if (section === 'timeline' && id) return { view: 'entry', project, id, nav: 'timeline' };
  if (section === 'timeline') return { view: 'timeline', project, nav: 'timeline' };
  return { view: 'dashboard', project, nav: 'dashboard' };
}

function shell(current) {
  const app = document.getElementById('app');
  const view = h('main', { class: 'view', id: 'view', tabindex: '-1' });
  const top = h('header', { class: 'topbar' },
    h('a', { class: 'brand', href: '#/' }, h('span', { class: 'brand-mark', 'aria-hidden': 'true', text: 'λ' }), h('span', { class: 'brand-name', text: 'lambda memory' })),
    themeSwitch());
  let nav = null;
  if (current.project) {
    const items = [['dashboard', 'Overview'], ['memories', 'Memories'], ['timeline', 'Decision timeline']];
    nav = h('nav', { class: 'sidenav', 'aria-label': 'Project' },
      h('div', { class: 'project-context' }, h('p', { class: 'eyebrow', text: 'Project' }),
        h('strong', { class: 'project-label', text: projectLabel(current.project), title: current.project }),
        h('p', { class: 'scope-note', text: 'Project isolated · all reads and changes stay in this project.' }),
        disclosure('Full project identity', mono(current.project))),
      h('ul', null, items.map(([key, label]) => h('li', null,
        h('a', { href: link(current.project, key), text: label, 'aria-current': current.nav === key ? 'page' : false })))),
      h('p', null, h('a', { href: '#/', class: 'muted', text: '← All projects' })),
      h('p', { class: 'scope-note', text: 'Local workspace · stored on this device' }));
  }
  app.replaceChildren(top, h('div', { class: nav ? 'layout' : 'layout no-nav' }, nav, view));
  return view;
}

const VIEWS = { projects: viewProjects, dashboard: viewDashboard, memories: viewMemories, memory: viewMemory, timeline: viewTimeline, entry: viewEntry };

async function render() {
  for (const fn of cleanups.splice(0)) fn();
  const gen = ++generation;
  const current = route();
  const view = shell(current);
  const ctx = { alive: () => gen === generation, project: current.project };
  try {
    if (current.project && !(await projectGate(view, ctx))) return;
    await VIEWS[current.view](view, ctx, current);
  } catch (error) {
    if (!ctx.alive()) return;
    if (error?.kind === 'needs_start') { view.replaceChildren(); startPanel(view, ctx.project); return; }
    view.append(failure(error));
  }
}

/** Confirms the project is registered and live; otherwise offers to start its worker. */
async function projectGate(view, ctx) {
  const { projects = [] } = await api('/api/projects');
  if (!ctx.alive()) return false;
  const entry = projects.find((item) => item.project === ctx.project);
  if (!entry) {
    view.append(h('h1', { text: 'Unknown project' }), notice('error', 'This project is not in the registry.', ctx.project), h('a', { href: '#/', text: 'Back to projects' }));
    return false;
  }
  if (!entry.live) { startPanel(view, ctx.project, entry); return false; }
  return true;
}

function startPanel(view, project, entry) {
  const errorBox = h('div');
  const button = h('button', { type: 'button', class: 'primary', text: 'Start worker' });
  button.addEventListener('click', async () => {
    button.disabled = true; errorBox.replaceChildren();
    try {
      await api('/api/start', { project });
      toast('ok', 'Worker started.');
      render();
    } catch (error) {
      errorBox.replaceChildren(failure(error));
      button.disabled = false;
    }
  });
  view.append(h('h1', { text: 'Worker not running' }), card(
    entry ? kv([['Project', mono(project)], ['Directory', mono(entry.cwd)], ['Last seen', timeEl(entry.last_seen_ms)]]) : kv([['Project', mono(project)]]),
    notice('warn', 'Starting the worker may process pending sources.', 'Pending captures can be consolidated while the worker runs, which is billable in openrouter mode. The worker stops after the UI lease expires.'),
    h('div', { class: 'row' }, button), errorBox));
}

// ---------------------------------------------------------------------------------------------
// Projects

async function viewProjects(view, ctx) {
  const refresh = h('button', { type: 'button', text: 'Refresh', onclick: () => render() });
  view.append(h('div', { class: 'view-heading row spread' }, h('h1', { text: 'Projects' }), refresh), h('p', { class: 'lede', text: 'Choose a local project. Its memories, source evidence and decision timeline stay isolated from other projects.' }));
  const { projects = [] } = await api('/api/projects');
  if (!ctx.alive()) return;
  if (projects.length === 0) {
    view.append(h('p', { class: 'empty', text: 'No projects registered yet. Projects appear once an agent connects through the daemon.' }));
    return;
  }
  const sorted = [...projects].sort((a, b) => (b.last_seen_ms ?? 0) - (a.last_seen_ms ?? 0));
  view.append(h('ul', { class: 'plain' }, sorted.map((item) => h('li', null, card(
    h('div', { class: 'row spread' }, h('h2', null, h('a', { href: link(item.project, 'dashboard'), text: projectLabel(item.project) })), item.live ? badge('live') : badge('offline')),
    h('p', { class: 'state-explanation', text: item.live ? 'Worker running. Open this project to read and curate its evidence.' : 'Worker stopped. Opening this project does not start it automatically.' }),
    h('p', { class: 'meta-line' }, 'Last seen ', timeEl(item.last_seen_ms)),
    disclosure('Project identity and directory', kv([['Project', mono(item.project)], ['Directory', mono(item.cwd)]])))))));
}

// ---------------------------------------------------------------------------------------------
// Dashboard

async function viewDashboard(view, ctx) {
  const project = ctx.project;
  const statusBox = h('div');
  const pollNote = h('p', { class: 'muted', 'aria-live': 'polite' });
  view.append(h('div', { class: 'view-heading row spread' }, h('h1', { text: 'Overview' }), pollNote), h('p', { class: 'lede', text: 'Recall readiness and processing for this project. Active memories remain provisional evidence, not verified truth.' }), statusBox);

  const filters = { operation: '', outcome: '' };
  const tbody = h('tbody');
  const more = h('button', { type: 'button', text: 'Load more', hidden: true });
  const select = (label, values, key) => h('label', { class: 'inline' }, label, h('select', {
    onchange: (event) => { filters[key] = event.target.value; reload(); },
  }, h('option', { value: '', text: 'All' }), values.map((value) => h('option', { value, text: value }))));
  const activityError = h('div');
  view.append(disclosure('Operational activity',
    h('p', { class: 'form-help', text: 'Read-only processing history. These observations do not start work or retry failed operations.' }),
    h('div', { class: 'row spread' }, h('h2', { text: 'Activity' }), h('div', { class: 'row' }, select('Operation', OPERATIONS, 'operation'), select('Outcome', OUTCOMES, 'outcome'))),
    activityError,
    h('div', { class: 'table-wrap' }, h('table', null, h('thead', null, h('tr', null,
      ['Started', 'Operation', 'Phase', 'Kind', 'Outcome', 'Reason', 'Duration', 'Counts'].map((name) => h('th', { scope: 'col', text: name })))), tbody)),
    h('div', { class: 'row' }, more)));

  let cursor = null;
  let pages = 0;
  async function loadStatus() {
    const status = await tool(project, 'memory_status', {});
    if (ctx.alive()) {
      const open = new Set([...statusBox.querySelectorAll('details[open]')].map((el) => el.dataset.statusSection));
      const focusedSection = document.activeElement?.closest('details[data-status-section]');
      const focusKey = focusedSection && statusBox.contains(focusedSection) ? focusedSection.dataset.statusSection : null;
      statusBox.replaceChildren(...statusCards(status));
      for (const el of statusBox.querySelectorAll('details[data-status-section]')) {
        el.open = open.has(el.dataset.statusSection);
        if (el.dataset.statusSection === focusKey) el.querySelector('summary').focus({ preventScroll: true });
      }
    }
  }
  async function loadActivity(reset) {
    const args = { limit: 20 };
    if (filters.operation) args.operations = [filters.operation];
    if (filters.outcome) args.outcomes = [filters.outcome];
    if (!reset && cursor) args.cursor = cursor;
    const page = await tool(project, 'memory_activity', args);
    if (!ctx.alive()) return;
    if (reset) tbody.replaceChildren();
    for (const entry of page.entries ?? []) tbody.append(activityRow(entry));
    if (reset && (page.entries ?? []).length === 0) tbody.append(h('tr', null, h('td', { colspan: '8', class: 'empty', text: 'No activity recorded.' })));
    cursor = page.next_cursor ?? null;
    pages = reset ? 1 : pages + 1;
    more.hidden = !cursor;
  }
  async function reload() {
    activityError.replaceChildren();
    try { await loadActivity(true); } catch (error) { activityError.replaceChildren(failure(error)); }
  }
  let paging = false;
  more.addEventListener('click', () => { paging = true; act(more, () => loadActivity(false)).finally(() => { paging = false; }); });

  await Promise.all([loadStatus(), loadActivity(true)]);
  pollNote.textContent = `Auto-refresh every ${POLL_MS / 1000} s while visible.`;

  // Polls only the two status tools, only while the page is visible, and stops on the first error.
  let busy = false;
  const timer = setInterval(async () => {
    if (document.hidden || busy || !ctx.alive()) return;
    busy = true;
    try {
      await loadStatus();
      if (pages <= 1 && !paging) await loadActivity(true); // keep loaded pages intact once the user paged
      if (ctx.alive()) pollNote.textContent = `Updated ${new Date().toLocaleTimeString()} · every ${POLL_MS / 1000} s while visible.`;
    } catch (error) {
      clearInterval(timer);
      if (!ctx.alive()) return;
      if (error?.kind === 'needs_start') { render(); return; }
      pollNote.replaceChildren(h('span', { text: `Auto-refresh stopped: ${describe(error)} ` }),
        h('button', { type: 'button', class: 'small', text: 'Reload', onclick: () => render() }));
    } finally { busy = false; }
  }, POLL_MS);
  onCleanup(() => clearInterval(timer));
}

function statusCards(status) {
  const lifecycle = status.lifecycle ?? {};
  const pending = status.pending ?? {};
  const worker = status.worker ?? {};
  const dream = worker.dreaming ?? {};
  const diagnostics = status.diagnostics ?? {};
  const diagnosticSection = (key, label, ...children) => { const el = disclosure(label, ...children); el.dataset.statusSection = key; return el; };
  const last = [['Last success', status.last_success], ['Last failure', status.last_failure], ['Last dream success', status.last_dream_success], ['Last dream failure', status.last_dream_failure]];
  return [
    card(h('div', { class: 'row spread' }, h('h2', { text: 'Memories' }), h('span', { class: 'muted' }, 'Mode ', mono(status.mode), ' · observed ', timeEl(status.observed_at_ms))),
      h('div', { class: 'grid' }, STATES.map((state) => h('a', { class: 'stat', href: link(status.project, 'memories'), onclick: () => { prefs.memoriesTab = state; } },
        h('div', { class: 'value', text: String(lifecycle[state] ?? 0) }), badge(state), stateMeaning(state))))),
    card(h('h2', { text: 'Pending work' }), h('p', { class: 'form-help', text: 'Automatic preparation, not a human approval queue. Blocked work needs investigation; this screen never retries it.' }), kv([
      ['Unconsumed sources', String(pending.unconsumed_sources ?? 0)],
      ['Processing revisions', String(pending.processing_revisions ?? 0)],
      ['Blocked sources', String(pending.blocked_sources ?? 0)],
      ['Blocked revisions', String(pending.blocked_revisions ?? 0)],
      ['Active without current embedding', String(pending.active_without_current_embedding ?? 0)],
      ['Unresolved conflicts', String(status.review?.unresolved_conflicts ?? 0)],
    ])),
    diagnosticSection('worker', 'Worker diagnostics', kv([
      ['Instance', mono(worker.instance)],
      ['Started', timeEl(worker.started_at_ms)],
      ['Foreground active', worker.foreground_active ? 'yes' : 'no'],
      ['Dreaming', h('span', { class: 'row' }, badge(dream.state), dream.phase ? mono(dream.phase) : null, dream.stop_reason ? h('span', { class: 'muted', text: `stopped: ${dream.stop_reason}` }) : null)],
      ['Idle threshold', fmtDuration(dream.idle_ms)],
      ['Remaining idle', fmtDuration(dream.remaining_idle_ms)],
      ['Tick / batch', `${fmtDuration(dream.tick_ms)} / ${dream.dream_batch ?? '—'}`],
      ['Recording', h('span', { class: 'row' }, badge(diagnostics.state), diagnostics.reason ? mono(diagnostics.reason) : null)],
      ['History', `${status.history?.retention_days ?? '—'} days retained since ${fmtTime(status.history?.retained_since_ms)}`],
    ])),
    diagnosticSection('outcomes', 'Recent operation outcomes', h('div', { class: 'table-wrap' }, h('table', null,
      h('thead', null, h('tr', null, ['', 'Operation', 'Outcome', 'Reason', 'Finished'].map((name) => h('th', { scope: 'col', text: name })))),
      h('tbody', null, last.map(([label, entry]) => h('tr', null, h('th', { scope: 'row', text: label }),
        entry ? [h('td', null, mono(`${entry.operation}/${entry.phase}`)), h('td', null, badge(entry.outcome)), h('td', null, entry.reason ? mono(entry.reason) : '—'), h('td', null, timeEl(entry.finished_at_ms ?? entry.started_at_ms))]
          : h('td', { colspan: '4', class: 'empty', text: 'none' }))))))),
  ];
}

function activityRow(entry) {
  const counts = Object.entries(entry.counts ?? {}).map(([key, value]) => `${key.replace(/_/g, ' ')}: ${value}`).join(', ');
  return h('tr', null,
    h('td', null, timeEl(entry.started_at_ms)),
    h('td', null, mono(entry.operation)),
    h('td', null, mono(entry.phase)),
    h('td', { text: entry.entry_kind }),
    h('td', null, badge(entry.outcome)),
    h('td', null, entry.reason ? mono(entry.reason) : '—'),
    h('td', { text: fmtDuration(entry.duration_ms) }),
    h('td', { class: 'muted', text: counts || '—' }));
}

// ---------------------------------------------------------------------------------------------
// Memories

async function viewMemories(view, ctx) {
  const project = ctx.project;
  const list = h('ul', { class: 'plain memory-list' });
  const more = h('button', { type: 'button', text: 'Load more', hidden: true });
  const errorBox = h('div');
  const meaning = stateMeaning(prefs.memoriesTab);
  const tabs = STATES.map((state) => h('button', {
    type: 'button', role: 'tab', text: state, 'aria-selected': String(state === prefs.memoriesTab),
    onclick: () => { prefs.memoriesTab = state; meaning.textContent = STATE_MEANINGS[state]; for (const tab of tabs) tab.setAttribute('aria-selected', String(tab.textContent === state)); load(true).catch(showError); },
  }));
  view.append(h('h1', { text: 'Memories' }), h('p', { class: 'lede', text: 'Read source-backed content, inspect its provenance, and make deliberate corrections.' }), h('div', { class: 'tabs', role: 'tablist', 'aria-label': 'Lifecycle state' }, tabs), meaning, errorBox, list, h('div', { class: 'row' }, more));

  let cursor = null;
  let token = 0;
  function showError(error) { if (ctx.alive()) errorBox.replaceChildren(failure(error)); }
  async function load(reset) {
    const mine = reset ? ++token : token;
    const state = prefs.memoriesTab;
    const args = { states: [state], limit: 32 };
    if (!reset && cursor) args.cursor = cursor;
    if (reset) { errorBox.replaceChildren(); list.replaceChildren(h('li', { class: 'muted', text: 'Loading…' })); more.hidden = true; }
    const page = await tool(project, 'memory_list', args);
    if (!ctx.alive() || mine !== token) return;
    if (reset) list.replaceChildren();
    for (const item of page.revisions ?? []) list.append(h('li', null, memoryCard(project, item)));
    if (reset && (page.revisions ?? []).length === 0) list.append(h('li', { class: 'empty', text: `No ${state} revisions.` }));
    if (page.truncated && !page.next_cursor) list.append(h('li', { class: 'muted', text: 'Results truncated by the response budget.' }));
    cursor = page.next_cursor ?? null;
    more.hidden = !cursor;
  }
  more.addEventListener('click', () => act(more, () => load(false)));
  await load(true);
}

function memoryCard(project, item) {
  return h('article', { class: `card revision state-${item.state}` },
    h('div', { class: 'meta-line row spread' }, badge(item.state), h('span', null, 'Updated ', timeEl(item.watermark_ms))),
    h('h2', { class: 'memory-title' }, h('a', { href: link(project, 'memory', item.memory), text: clip(item.content ?? '', 240) || 'Open memory' })),
    h('p', { class: 'meta-line', text: `${(item.sources ?? []).length} source(s) · content excerpt` }),
    disclosure('Technical identifiers', kv([['Memory', mono(item.memory)], ['Revision', mono(item.revision)]])));
}

async function loadAll(fetchPage, key, ctx) {
  const items = [];
  let cursor;
  for (let page = 0; page < MAX_PAGES; page += 1) {
    const body = await fetchPage(cursor);
    if (!ctx.alive()) return { items, complete: false };
    items.push(...(body[key] ?? []));
    cursor = body.next_cursor;
    if (!cursor) return { items, complete: true };
  }
  return { items, complete: false };
}

async function viewMemory(view, ctx, current) {
  const project = ctx.project;
  const memory = current.id;
  view.append(h('p', null, h('a', { href: link(project, 'memories'), text: '← Memories' })), h('h1', { text: 'Memory detail' }));
  const { items, complete } = await loadAll((cursor) => tool(project, 'memory_list', { memory, states: STATES, limit: 32, ...(cursor ? { cursor } : {}) }), 'revisions', ctx);
  if (!ctx.alive()) return;
  if (items.length === 0) { view.append(h('p', { class: 'empty', text: 'No revisions found for this memory.' })); return; }
  if (!complete) view.append(notice('warn', 'Showing the first revisions only.', 'This memory has more revisions than the UI loads at once.'));
  const byId = new Map(items.map((item) => [item.revision, item]));
  const active = items.find((item) => item.state === 'active');
  const cards = new Map();
  const jump = (id) => () => {
    const el = cards.get(id);
    if (!el) return;
    for (let ancestor = el.parentElement; ancestor; ancestor = ancestor.parentElement) {
      if (ancestor.tagName === 'DETAILS') ancestor.open = true;
    }
    el.scrollIntoView({ block: 'start' });
    el.focus({ preventScroll: true });
  };

  // Lineage: each chain runs from its oldest known revision to a head nobody succeeds.
  const succeeded = new Set(items.map((item) => item.predecessor).filter(Boolean));
  const chains = items.filter((item) => !succeeded.has(item.revision)).map((head) => {
    const chain = [];
    const seen = new Set();
    for (let node = head; node && !seen.has(node.revision); node = node.predecessor ? byId.get(node.predecessor) : null) { seen.add(node.revision); chain.unshift(node); }
    return { chain, missing: chain[0].predecessor && !byId.has(chain[0].predecessor) ? chain[0].predecessor : null };
  }).sort((a, b) => Number(b.chain.some((n) => n.state === 'active')) - Number(a.chain.some((n) => n.state === 'active')) || b.chain.length - a.chain.length);
  const lineage = disclosure('Revision lineage', chains.map(({ chain, missing }) => h('div', { class: 'lineage' },
    missing ? [h('span', { class: 'muted' }, mono(missing), ' (unavailable)'), h('span', { class: 'arrow', 'aria-hidden': 'true', text: '→' })] : null,
    chain.map((node, index) => [index ? h('span', { class: 'arrow', 'aria-hidden': 'true', text: '→' }) : null,
      h('button', { type: 'button', class: 'small', onclick: jump(node.revision), 'aria-label': `Revision ${node.revision}, ${node.state}` }, mono(node.revision), ' ', badge(node.state))]))));

  const sorted = [...items].sort((a, b) => (b.watermark_ms ?? 0) - (a.watermark_ms ?? 0));
  const currentRevision = active ?? sorted[0];
  const featured = revisionCard(project, currentRevision, items, active, byId, jump);
  cards.set(currentRevision.revision, featured);
  const history = disclosure(`Other revisions (${items.length - 1})`,
    h('p', { class: 'form-help', text: 'Past and nonactive revisions remain available for comparison. Each revision keeps its own evidence and lifecycle audit.' }));
  const historyBody = history.querySelector('.disclosure-body');
  for (const item of sorted) {
    if (item === currentRevision) continue;
    const el = revisionCard(project, item, items, active, byId, jump);
    cards.set(item.revision, el);
    historyBody.append(disclosure(`${item.state} · ${fmtTime(item.watermark_ms)} · ${item.revision}`, el));
  }
  view.append(h('div', { class: 'reading-layout' },
    h('div', { class: 'reading-main stack' },
      h('p', { class: 'revision-label', text: active ? 'Current active revision' : 'Newest loaded revision · no active revision is available' }),
      !active ? notice('warn', 'This is not active recall content.', 'Read the lifecycle state before using it. Only processing or conflicted revisions can be promoted.') : null,
      featured,
      disclosure('Correct this memory', correctCard(project, memory, active)),
      history, lineage),
    h('aside', { class: 'context-rail stack', 'aria-label': 'Memory context' },
      card(h('h2', { text: 'Recall state' }), badge(currentRevision.state), stateMeaning(currentRevision.state)),
      card(h('h2', { text: 'Source evidence' }), h('p', { text: `${(currentRevision.sources ?? []).length} source(s) support this revision.` }),
        h('p', { class: 'form-help', text: 'Sources preserve the original event and outcome. Successful processing does not turn a failed or unknown event into verified truth. Expand source provenance below the content for exact evidence identifiers.' })),
      disclosure('Memory identifiers', kv([['Project', mono(project)], ['Memory', mono(memory)], ['Displayed revision', mono(currentRevision.revision)]])))));
}

function correctCard(project, memory, active) {
  const attempt = { content: null, nonce: null };
  if (!active) return card(h('h2', { text: 'Correct this memory' }), h('p', { class: 'muted', text: 'There is no active revision to correct. Resolve or promote a revision first.' }));
  const textarea = h('textarea', { id: 'correct-content', value: active.content ?? '', required: true });
  const button = h('button', { type: 'submit', class: 'primary', text: 'Propose correction' });
  const form = h('form', null, h('h2', { text: 'Correct this memory' }),
    h('p', { class: 'form-help' }, 'Preserves the existing evidence, records a manual-curation decision source and proposes a new revision whose predecessor is the active revision ', mono(active.revision), '.'),
    h('label', { for: 'correct-content' }, 'Corrected content'), textarea, h('div', { class: 'row' }, button));
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    const content = textarea.value.trim();
    if (!content) { toast('error', 'The corrected content is empty.'); return; }
    if (content === (active.content ?? '').trim()) { toast('error', 'The content is unchanged.'); return; }
    act(button, async () => {
      // One nonce per content: a retry after "not confirmed" replays the same source and revision.
      if (attempt.content !== content) { attempt.content = content; attempt.nonce = crypto.randomUUID(); }
      const out = await api('/api/correct', { project, memory, predecessor: active.revision, content, nonce: attempt.nonce });
      if (out?.capture?.isError) throw new ApiError('tool', `Capture failed: ${errorText(out.capture.body)}`, 200, out.capture.body);
      if (out?.propose?.isError) throw new ApiError('tool', `The source was captured, but the proposal failed: ${errorText(out.propose.body)}`, 200, out.propose.body);
      render();
      return out.propose?.body;
    }, (revision) => `Correction proposed as ${revision?.revision ?? 'a new revision'} (${revision?.state ?? 'processing'}).`);
  });
  return h('section', { class: 'card' }, form);
}

function revisionCard(project, item, items, active, byId, jump) {
  const open = item.state === 'processing' || item.state === 'conflicted';
  const others = items.filter((other) => other.revision !== item.revision && ['active', 'processing', 'conflicted'].includes(other.state));
  const review = (args) => tool(project, 'memory_review', { revision: item.revision, ...args });
  const done = (message) => async () => { render(); return message; };
  const actions = [];
  if (open) {
    actions.push(h('button', {
      type: 'button', text: item.state === 'conflicted' ? 'Resolve (promote)' : 'Promote',
      onclick: () => actionDialog({
        title: `Promote ${item.revision}?`, confirmLabel: 'Promote',
        body: [
          h('p', null, 'This revision becomes active. ', active ? h('span', null, 'The current active revision ', mono(active.revision), ' is checked again before replacement and becomes superseded.') : 'There is no active revision.'),
          h('p', { text: 'Resolving a conflict also rejects the counterpart. Sources and history remain stored. Preparation may make billable model calls in OpenRouter mode; availability does not certify the claim as true.' }),
        ],
        run: async () => { await review({ action: 'promote', ...(active ? { expected_active: active.revision } : {}) }); return done(`Promoted ${item.revision}.`)(); },
      }),
    }));
    if (others.length) {
      actions.push(h('button', {
        type: 'button', text: 'Declare conflict',
        onclick: () => {
          const choice = h('select', { id: 'conflict-with' }, others.map((other) => h('option', { value: other.revision, text: `${other.revision} (${other.state})`, selected: other === active })));
          actionDialog({
            title: `Declare a conflict for ${item.revision}`, confirmLabel: 'Declare conflict',
            body: [h('p', { text: 'Both revisions become conflicted and unavailable for recall until the conflict is resolved. Sources and history remain stored.' }), h('label', { for: 'conflict-with' }, 'Counterpart revision', choice)],
            run: async () => { await review({ action: 'conflict', conflict_with: choice.value }); return done(`Conflict declared with ${choice.value}.`)(); },
          });
        },
      }));
    }
    actions.push(h('button', {
      type: 'button', class: 'danger', text: 'Reject',
      onclick: () => actionDialog({
        title: `Reject ${item.revision}?`, confirmLabel: 'Reject', danger: true,
        body: [h('p', { text: 'This revision stays out of recall and will no longer be prepared. Its sources and revision history remain stored. An active revision cannot be rejected.' })],
        run: async () => { await review({ action: 'reject' }); return done(`Rejected ${item.revision}.`)(); },
      }),
    }));
  }
  const predecessor = item.predecessor
    ? (byId.has(item.predecessor) ? h('button', { type: 'button', class: 'small', onclick: jump(item.predecessor) }, mono(item.predecessor)) : h('span', null, mono(item.predecessor), ' (unavailable)'))
    : 'none';
  const audit = item.audit ?? [];
  return h('article', { class: `card reading-card revision state-${item.state}`, tabindex: '-1' },
    h('div', { class: 'row spread' }, h('h2', { text: 'Content' }), badge(item.state)),
    h('p', { class: 'content prose', text: item.content ?? '' }),
    stateMeaning(item.state),
    h('p', { class: 'meta-line' }, 'Updated ', timeEl(item.watermark_ms)),
    actions.length ? disclosure('Lifecycle actions',
      h('p', { class: 'form-help', text: 'Promotion makes this revision active and supersedes the current active revision. Declaring a conflict marks both selected revisions as conflicted. Rejection keeps source evidence but excludes this revision from recall.' }),
      h('div', { class: 'action-stack' }, actions)) : null,
    disclosure(`Source provenance (${(item.sources ?? []).length})`,
      h('p', { class: 'form-help', text: 'Original source kind and outcome are preserved. Identifiers below are exact; provenance is not a truth guarantee.' }),
      h('ul', { class: 'plain source-list' }, (item.sources ?? []).map((source) => h('li', { class: 'source-card' },
        h('div', { class: 'row' }, badge(source.kind), badge(source.outcome)),
        kv([['Origin', source.origin ?? '—'], ['Captured', timeEl(source.recorded_at_ms)], ['Source', mono(source.source)]]),
        h('div', { class: 'danger-zone' },
          h('p', { class: 'form-help', text: 'Permanent deletion also removes derived revisions and timeline entries that depend on this source.' }),
          h('button', { type: 'button', class: 'danger small', text: 'Purge source', onclick: () => purgeDialog(project, source.source) })))))),
    disclosure('Technical identifiers and predecessor', kv([
      ['Memory', mono(item.memory)], ['Revision', mono(item.revision)], ['Predecessor', predecessor], ['Reviewed', timeEl(item.reviewed_at_ms)],
    ])),
    disclosure(`Lifecycle audit (${audit.length})`, audit.length ? h('div', { class: 'table-wrap' }, h('table', null,
      h('thead', null, h('tr', null, ['When', 'Action', 'Transition', 'Expected active', 'Conflict with'].map((name) => h('th', { scope: 'col', text: name })))),
      h('tbody', null, audit.map((row) => h('tr', null, h('td', null, timeEl(row.at_ms)), h('td', null, mono(row.action)),
        h('td', null, badge(row.from_state), ' → ', badge(row.to_state)),
        h('td', null, row.expected_active ? mono(row.expected_active) : '—'), h('td', null, row.conflict_with ? mono(row.conflict_with) : '—'))))))
      : h('p', { class: 'empty', text: 'No lifecycle reviews recorded.' })));
}

function purgeDialog(project, source) {
  actionDialog({
    title: 'Purge source', confirmLabel: 'Purge permanently', danger: true, typed: source,
    body: [
      h('p', null, 'Source ', mono(source)),
      notice('warn', 'This cannot be undone.', 'Purging deletes the source and, in cascade, every revision derived from it and every timeline entry (with all its versions) that cites the source or a removed revision.'),
    ],
    run: async (typed) => {
      const result = await api('/api/purge', { project, sources: [source], confirm: typed });
      if (result?.isError) throw new ApiError('tool', errorText(result.body), 200, result.body);
      const r = result.body ?? {};
      render();
      return `Purged ${r.sources ?? 0} source(s), ${r.revisions ?? 0} revision(s), ${r.jobs ?? 0} job(s) and ${r.timeline_entries ?? 0} timeline entr${r.timeline_entries === 1 ? 'y' : 'ies'}.`;
    },
  });
}

// ---------------------------------------------------------------------------------------------
// Timeline

function slug(title) {
  const base = title.normalize('NFKD').replace(/[̀-ͯ]/g, '').toLowerCase()
    .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 64).replace(/-+$/, '');
  return base || 'entry';
}
function toLocalInput(ms) {
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}
/** Cuts to at most `max` UTF-8 bytes on a code-point boundary, marking the cut with an ellipsis. */
function clip(text, max) {
  if (bytes(text) <= max) return text;
  let out = '';
  for (const char of text) { if (bytes(`${out}${char}…`) > max) break; out += char; }
  return `${out}…`;
}
const splitIds = (text) => [...new Set(text.split(',').map((part) => part.trim()).filter(Boolean))];
function entryArgs(entry) {
  const args = {
    entry: entry.entry, title: entry.title, summary: entry.summary, occurred_at_ms: entry.occurred_at_ms, status: entry.status,
    sources: entry.sources ?? [], revisions: entry.revisions ?? [], memories: entry.memories ?? [], tags: entry.tags ?? [],
  };
  if (entry.supersedes) args.supersedes = entry.supersedes;
  return args;
}
const sameArgs = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/** Create (no `entry`) or edit (`entry` = head version) a timeline entry. */
function entryDialog(project, { entry, prefill = {}, onDone }) {
  const base = entry ? entryArgs(entry) : {
    title: prefill.title ?? '', summary: prefill.summary ?? '', occurred_at_ms: prefill.occurred_at_ms ?? Date.now(),
    status: 'adopted', sources: prefill.sources ?? [], revisions: [], memories: [], tags: [],
  };
  const initialWhen = toLocalInput(base.occurred_at_ms);
  // The new entry's ID is fixed on the first submit so a retry replays instead of duplicating.
  let frozenId = null;
  const idNote = h('p', { class: 'muted', text: 'Entry ID: assigned from the title on the first save, then kept for retries.' });
  const field = (id, label, control, hint) => h('label', { for: id }, h('span', null, label, hint ? h('span', { class: 'hint', text: ` — ${hint}` }) : null), control);
  const title = h('input', { id: 'tl-title', type: 'text', value: base.title, required: true, maxLength: 200 });
  const summary = h('textarea', { id: 'tl-summary', value: base.summary, required: true });
  const when = h('input', { id: 'tl-when', type: 'datetime-local', value: initialWhen, required: true });
  const status = h('select', { id: 'tl-status' }, TIMELINE_STATUSES.map((value) => h('option', { value, text: value, selected: value === base.status })));
  const statusHelp = stateMeaning(base.status, true);
  status.addEventListener('change', () => { statusHelp.textContent = TIMELINE_MEANINGS[status.value]; });
  const sources = h('input', { id: 'tl-sources', type: 'text', class: 'mono', value: base.sources.join(', '), required: true });
  const revisions = h('input', { id: 'tl-revisions', type: 'text', class: 'mono', value: base.revisions.join(', ') });
  const memories = h('input', { id: 'tl-memories', type: 'text', class: 'mono', value: base.memories.join(', ') });
  const supersedes = h('input', { id: 'tl-supersedes', type: 'text', class: 'mono', value: base.supersedes ?? '' });
  const tags = h('input', { id: 'tl-tags', type: 'text', class: 'mono', value: base.tags.join(', ') });
  const body = h('div', { class: 'fields' },
    h('p', { class: 'form-help', text: entry ? 'Save appends a new version without rewriting earlier history. If another edit has changed this entry, reload before editing again.' : 'Record a curated decision backed by sources from this project. This does not approve memories or change their recall state.' }),
    prefill.truncated ? notice('warn', 'The source preview was truncated.', 'Review the summary before saving.') : null,
    field('tl-title', 'Title', title, 'at most 200 bytes'),
    field('tl-summary', 'Summary', summary),
    h('div', { class: 'grid' }, field('tl-when', 'Occurred at', when), field('tl-status', 'Status', status)),
    statusHelp,
    field('tl-sources', 'Source evidence', sources, 'required, exact source IDs from this project, comma-separated'),
    disclosure('Advanced links, tags and identifiers',
      entry ? h('p', { class: 'muted' }, 'Entry ', mono(entry.entry), ` · editing version ${entry.version}`) : idNote,
      field('tl-revisions', 'Revisions', revisions, 'optional, comma-separated'),
      field('tl-memories', 'Memories', memories, 'optional, comma-separated'),
      field('tl-supersedes', 'Supersedes entry', supersedes, 'optional entry ID'),
      field('tl-tags', 'Tags', tags, 'optional, lowercase, comma-separated')));
  actionDialog({
    title: entry ? 'Edit timeline entry' : 'New timeline entry', confirmLabel: entry ? 'Save new version' : 'Create entry', body: [body],
    run: async () => {
      const titleText = title.value.trim();
      const occurred = when.value === initialWhen ? base.occurred_at_ms : new Date(when.value).getTime();
      const args = {
        entry: entry ? entry.entry : (frozenId ?? `${slug(titleText)}-${Date.now().toString(36)}`),
        title: titleText, summary: summary.value.trim(), occurred_at_ms: occurred, status: status.value,
        sources: splitIds(sources.value), revisions: splitIds(revisions.value), memories: splitIds(memories.value), tags: splitIds(tags.value),
      };
      if (supersedes.value.trim()) args.supersedes = supersedes.value.trim();
      const invalid = (message) => new ApiError('invalid', message, 0, null);
      if (!args.title || bytes(args.title) > 200) throw invalid('The title is required and must be at most 200 bytes.');
      if (!args.summary) throw invalid('The summary is required.');
      if (!Number.isFinite(occurred)) throw invalid('Choose when the decision occurred.');
      if (args.sources.length < 1 || args.sources.length > 32) throw invalid('Give between 1 and 32 source IDs.');
      if (args.revisions.length > 32 || args.memories.length > 32) throw invalid('At most 32 revisions and 32 memories.');
      if (args.tags.length > 16 || args.tags.some((tag) => !TAG_PATTERN.test(tag))) throw invalid('At most 16 tags, each matching a-z, 0-9, ".", "_" or "-" (starting with a letter or digit).');
      if (args.supersedes === args.entry) throw invalid('An entry cannot supersede itself.');
      if (entry) {
        if (sameArgs(args, entryArgs(entry))) throw invalid('No changes to save.');
        args.expected_version = entry.version;
      }
      if (!entry) { frozenId = args.entry; idNote.replaceChildren('Entry ID ', mono(args.entry), ' (kept if you retry)'); }
      const receipt = await tool(project, 'memory_timeline_write', args);
      onDone?.(receipt.entry);
      return receipt.write === 'already_recorded' ? 'Already recorded; nothing changed.' : `Recorded ${receipt.entry?.entry} version ${receipt.entry?.version}.`;
    },
  });
}

function retractDialog(project, head) {
  actionDialog({
    title: `Retract “${head.title}”?`, confirmLabel: 'Retract', danger: true,
    body: [h('p', { text: `This writes version ${head.version + 1} with status “retracted”. The history is kept and the entry still covers its sources.` })],
    run: async () => {
      const receipt = await tool(project, 'memory_timeline_write', { ...entryArgs(head), status: 'retracted', expected_version: head.version });
      render();
      return `Retracted ${receipt.entry?.entry} (version ${receipt.entry?.version}).`;
    },
  });
}

function entryButtons(project, head) {
  return h('div', { class: 'row' },
    h('button', { type: 'button', class: 'small', text: 'Edit entry', title: 'Save changes as a new version; keep previous history', onclick: () => entryDialog(project, { entry: head, onDone: () => render() }) }),
    head.status === 'retracted' ? null : h('button', { type: 'button', class: 'small danger', text: 'Retract', title: 'Append a retracted version; keep history and source coverage', onclick: () => retractDialog(project, head) }));
}

async function viewTimeline(view, ctx) {
  const project = ctx.project;
  const order = h('button', { type: 'button', 'aria-pressed': String(prefs.timelineOrder === 'desc') });
  const setOrderLabel = () => { order.textContent = prefs.timelineOrder === 'asc' ? 'Order: oldest first' : 'Order: newest first'; };
  setOrderLabel();
  order.addEventListener('click', () => { prefs.timelineOrder = prefs.timelineOrder === 'asc' ? 'desc' : 'asc'; render(); });
  const filter = h('fieldset', null, h('legend', { text: 'Status' }), TIMELINE_STATUSES.map((value) => h('label', { class: 'inline' },
    h('input', { type: 'checkbox', checked: prefs.timelineStatuses.has(value), onchange: (event) => { if (event.target.checked) prefs.timelineStatuses.add(value); else prefs.timelineStatuses.delete(value); render(); } }), badge(value))));
  const create = h('button', { type: 'button', class: 'primary', text: 'Record decision', onclick: () => entryDialog(project, { onDone: (entry) => { location.hash = link(project, 'timeline', entry.entry); } }) });
  const uncovered = h('div');
  const list = h('ol', { class: 'timeline' });
  const more = h('button', { type: 'button', text: 'Load more', hidden: true });
  view.append(h('div', { class: 'view-heading row spread' }, h('h1', { text: 'Decision timeline' }), create), h('p', { class: 'lede', text: 'A curated history of this project’s decisions, separate from automatic memory processing.' }), h('div', { class: 'row' }, order, filter), list, h('div', { class: 'row' }, more), uncovered);

  const known = new Map();
  const refs = [];
  let cursor = null;
  const filtered = prefs.timelineStatuses.size > 0;
  function resolveRefs() {
    for (const ref of refs) {
      const target = known.get(ref.target);
      if (target) ref.el.replaceChildren('Supersedes ', h('a', { href: link(project, 'timeline', ref.target), text: target.title }));
      else if (!cursor && !filtered) ref.el.replaceChildren('Supersedes ', mono(ref.target), ' (unavailable)');
      else ref.el.replaceChildren('Supersedes ', h('a', { href: link(project, 'timeline', ref.target) }, mono(ref.target)));
    }
  }
  async function load(first) {
    const args = { limit: 32, order: prefs.timelineOrder };
    if (filtered) args.statuses = [...prefs.timelineStatuses];
    if (!first && cursor) args.cursor = cursor;
    const page = await tool(project, 'memory_timeline', args);
    if (!ctx.alive()) return;
    if (first) uncovered.replaceChildren(uncoveredPanel(project, page.uncovered_decisions ?? [], page.uncovered_truncated) ?? '');
    for (const entry of page.entries ?? []) {
      known.set(entry.entry, entry);
      const ref = entry.supersedes ? h('p', { class: 'muted' }) : null;
      if (ref) refs.push({ target: entry.supersedes, el: ref });
      list.append(h('li', { class: `status-${entry.status}` }, card(
        h('div', { class: 'row spread' }, timeEl(entry.occurred_at_ms), h('span', { class: 'row' }, badge(entry.status), h('span', { class: 'muted', text: `v${entry.version}` }))),
        h('h3', null, h('a', { href: link(project, 'timeline', entry.entry), text: entry.title })),
        h('p', { class: 'content prose clamp', text: clip(entry.summary, 480) }),
        h('p', { class: 'meta-line' }, 'Summary preview · ',
          h('a', { href: link(project, 'timeline', entry.entry), text: 'Read full decision', 'aria-label': `Read full decision: ${entry.title}` })),
        stateMeaning(entry.status, true),
        (entry.tags ?? []).length ? h('div', { class: 'tags' }, entry.tags.map((tag) => h('span', { class: 'tag', text: tag }))) : null,
        ref,
        disclosure('Source links and technical details',
          kv([['Entry', mono(entry.entry)], ['Recorded', timeEl(entry.recorded_at_ms)]]),
          h('p', { class: 'form-help', text: `${(entry.sources ?? []).length} source(s). Open the entry to read all versions and evidence links.` }),
          h('a', { href: link(project, 'timeline', entry.entry), text: 'Read entry and version history' })),
        entryButtons(project, entry))));
    }
    if (first && (page.entries ?? []).length === 0) list.append(h('li', { class: 'empty', text: filtered ? 'No entries with these statuses.' : 'No timeline entries yet.' }));
    cursor = page.next_cursor ?? null;
    more.hidden = !cursor;
    resolveRefs();
  }
  more.addEventListener('click', () => act(more, () => load(false)));
  await load(true);
}

function uncoveredPanel(project, decisions, truncated) {
  if (decisions.length === 0) return null;
  const panel = disclosure(`Captured decisions not yet on the timeline (${decisions.length}${truncated ? '+' : ''})`,
    h('p', { class: 'form-help', text: `These captured decisions are not cited by any timeline entry${truncated ? ' (newest 16 shown)' : ''}. This is a separate curated record, not an automatic truth-approval queue.` }),
    h('ul', { class: 'plain' }, decisions.map((decision) => h('li', { class: 'card source-card' },
      h('div', { class: 'row spread' }, badge(decision.outcome), timeEl(decision.recorded_at_ms)),
      h('p', { class: 'content prose', text: `${decision.preview ?? ''}${decision.preview_truncated ? ' …' : ''}` }),
      disclosure('Source provenance', kv([['Source', mono(decision.source)], ['Origin', decision.origin ?? '—']])),
      h('button', {
        type: 'button', class: 'primary small', text: 'Record decision from this source',
        onclick: () => {
          const preview = (decision.preview ?? '').trim();
          const firstLine = clip(preview.split('\n')[0].replace(/[\u0000-\u001f\u007f-\u009f]/g, ' ').trim(), 200);
          entryDialog(project, {
            prefill: { sources: [decision.source], title: firstLine, summary: preview, occurred_at_ms: decision.recorded_at_ms, truncated: decision.preview_truncated },
            onDone: (entry) => { location.hash = link(project, 'timeline', entry.entry); },
          });
        },
      }),
      disclosure('Permanent source deletion', h('div', { class: 'danger-zone' },
        h('p', { class: 'form-help', text: 'Deletes this evidence and all dependent revisions and timeline entries. This cannot be undone.' }),
        h('button', { type: 'button', class: 'danger small', text: 'Purge source', onclick: () => purgeDialog(project, decision.source) })))))));
  panel.open = prefs.uncoveredOpen;
  panel.addEventListener('toggle', () => { if (panel.isConnected) prefs.uncoveredOpen = panel.open; });
  return panel;
}

async function viewEntry(view, ctx, current) {
  const project = ctx.project;
  view.append(h('p', null, h('a', { href: link(project, 'timeline'), text: '← Timeline' })));
  const { items: versions, complete } = await loadAll((cursor) => tool(project, 'memory_timeline', { entry: current.id, limit: 32, ...(cursor ? { cursor } : {}) }), 'entries', ctx);
  if (!ctx.alive()) return;
  if (versions.length === 0) {
    view.append(h('h1', null, mono(current.id), ' (unavailable)'), h('p', { class: 'empty', text: 'This entry does not exist in the project, or it was deleted by a purge.' }));
    return;
  }
  if (!complete) view.append(notice('warn', 'History is incomplete.', 'Editing is disabled until every version can be loaded.'));
  const head = versions[versions.length - 1];
  const supersedes = h('span', null, head.supersedes ? mono(head.supersedes) : 'none');
  const linkList = (ids, section) => (ids?.length ? h('ul', { class: 'plain' }, ids.map((id) => h('li', null, section ? h('a', { href: link(project, section, id) }, mono(id)) : mono(id)))) : '—');
  const main = h('div', { class: 'reading-main stack' },
    h('article', { class: 'card reading-card' },
      h('p', { class: 'revision-label', text: complete ? `Current version · ${head.version}` : `Latest loaded version · ${head.version} · history incomplete` }),
      h('p', { class: 'meta-line' }, 'Occurred ', timeEl(head.occurred_at_ms)),
      h('p', { class: 'content prose', text: head.summary }),
      (head.tags ?? []).length ? h('div', { class: 'tags' }, head.tags.map((tag) => h('span', { class: 'tag', text: tag }))) : null),
    disclosure('Source evidence and memory links',
      h('p', { class: 'form-help', text: 'Exact links to this project’s evidence. Curation does not certify the sources as truth or change memory lifecycle states.' }),
      kv([['Sources', linkList(head.sources)], ['Revisions', linkList(head.revisions)], ['Memories', linkList(head.memories, 'memory')]])),
    disclosure('Technical identifiers', kv([['Entry', mono(head.entry)], ['Recorded', timeEl(head.recorded_at_ms)], ['Supersedes', supersedes]])));
  view.append(
    h('div', { class: 'view-heading row spread' }, h('h1', { text: head.title }), badge(head.status)),
    h('div', { class: 'reading-layout' }, main,
      h('aside', { class: 'context-rail stack', 'aria-label': 'Decision context' },
        card(h('h2', { text: 'Decision state' }), badge(head.status), stateMeaning(head.status, true)),
        complete ? card(h('h2', { text: 'Maintain this entry' }),
          h('p', { class: 'form-help', text: 'Editing saves a new version. Retracting withdraws the decision but keeps its history and source coverage. Neither action deletes evidence.' }),
          entryButtons(project, head)) : null)));
  if (head.supersedes) {
    try {
      const target = await tool(project, 'memory_timeline', { entry: head.supersedes, limit: 1 });
      if (ctx.alive()) {
        // limit 1 returns version 1, enough to know the entry exists; its title may be stale, so show only the ID.
        const found = (target.entries ?? [])[0];
        supersedes.replaceChildren(found ? h('a', { href: link(project, 'timeline', head.supersedes) }, mono(head.supersedes)) : h('span', null, mono(head.supersedes), ' (unavailable)'));
      }
    } catch (error) { if (ctx.alive()) supersedes.append(h('span', { class: 'muted', text: ` (could not check: ${describe(error)})` })); }
  }
  main.append(disclosure(`Version history (${versions.length}${complete ? '' : '+'})`,
    h('p', { class: 'form-help', text: 'Chronological versions, including the displayed version. Earlier text and evidence links remain unchanged.' }),
    h('ol', { class: 'timeline history-list' }, versions.map((version) => h('li', { class: `status-${version.status}` },
      disclosure(`Version ${version.version} · ${version.status} · ${fmtTime(version.recorded_at_ms)}`,
        h('h3', { text: version.title }),
        h('p', { class: 'meta-line' }, 'Occurred ', timeEl(version.occurred_at_ms)),
        h('p', { class: 'content prose', text: version.summary }),
        stateMeaning(version.status, true),
        disclosure('Version provenance and identifiers', kv([
          ['Entry', mono(version.entry)], ['Recorded', timeEl(version.recorded_at_ms)],
          ['Supersedes', version.supersedes ? h('a', { href: link(project, 'timeline', version.supersedes) }, mono(version.supersedes)) : '—'],
          ['Sources', linkList(version.sources)], ['Revisions', linkList(version.revisions)], ['Memories', linkList(version.memories, 'memory')],
          ['Tags', (version.tags ?? []).join(', ') || '—'],
        ]))))))));
}

// ---------------------------------------------------------------------------------------------

window.addEventListener('hashchange', () => { render().then(() => document.getElementById('view')?.focus({ preventScroll: true })); });
if (!location.hash) history.replaceState(null, '', '#/');
render();

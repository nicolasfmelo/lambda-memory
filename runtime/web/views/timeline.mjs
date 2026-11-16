import { html, useContext, useEffect, useRef, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { ApiContext } from '../components/nav.mjs';
import { Badge } from '../components/badge.mjs';
import { ActionDialog } from '../components/dialog.mjs';
import { Empty, ErrorNote, Loading, NeedsStart } from '../components/states.mjs';
import { errorText } from '../lib/api.mjs';
import { clip, fmtTime } from '../lib/format.mjs';
import { href } from '../lib/router.mjs';
import { PurgeAction } from './review.mjs';

const STATUSES = ['proposed', 'adopted', 'superseded', 'reverted', 'retracted'];
const MEANINGS = {
  proposed: 'A proposed decision, not yet adopted.',
  adopted: 'Recorded as an adopted project decision, not independently verified truth.',
  superseded: 'Recorded as replaced by another decision.',
  reverted: 'Recorded as a decision that was reversed.',
  retracted: 'Withdrawn from the curated account; history and source coverage remain.',
};
const TAG_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const AUTO_TITLE = 'Written automatically from a decision; a manual edit detaches it.';
const prefs = { order: 'asc', statuses: [], uncovered: false };
const bytes = value => new TextEncoder().encode(value).length;
const split = value => [...new Set(value.split(',').map(part => part.trim()).filter(Boolean))];

function slug(title) {
  return title.normalize('NFKD').replace(/[\u0300-\u036f]/g, '').toLowerCase()
    .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 64).replace(/-+$/, '') || 'entry';
}
function localTime(ms) {
  const date = new Date(ms);
  const p = n => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${p(date.getMonth() + 1)}-${p(date.getDate())}T` +
    `${p(date.getHours())}:${p(date.getMinutes())}`;
}
function entryArgs(entry) {
  const args = {
    entry: entry.entry, title: entry.title, summary: entry.summary, occurred_at_ms: entry.occurred_at_ms,
    status: entry.status, sources: entry.sources ?? [], revisions: entry.revisions ?? [],
    memories: entry.memories ?? [], tags: entry.tags ?? [],
  };
  if (entry.supersedes) args.supersedes = entry.supersedes;
  return args;
}
function entryHref(project, entry) {
  return href({ project, page: 'entry', id: entry });
}
function Tags({ tags }) {
  return html`<div class="timeline-tags">${(tags ?? []).map(tag => tag === 'origin-auto'
    ? html`<${Badge} title=${AUTO_TITLE}>auto</${Badge}>`
    : html`<${Badge}>${tag}</${Badge}>`)}</div>`;
}
function Pairs({ rows }) {
  return html`<dl class="timeline-pairs">${rows.map(([label, value]) => html`
    <div><dt>${label}</dt><dd>${value ?? '—'}</dd></div>`)}</dl>`;
}
function Links({ project, ids, page }) {
  if (!ids?.length) return '—';
  return html`<ul class="timeline-links">${ids.map(id => html`<li>
    ${page ? html`<a href=${href({ project, page, id })}><code>${id}</code></a>` : html`<code>${id}</code>`}
  </li>`)}</ul>`;
}

function EntryDialog({ project, entry, prefill = {}, onClose, onDone }) {
  const api = useContext(ApiContext);
  const opened = useRef({ project, entry, prefill }).current;
  project = opened.project;
  entry = opened.entry;
  prefill = opened.prefill;
  const base = useRef(entry ? entryArgs(entry) : {
    title: prefill.title ?? '', summary: prefill.summary ?? '',
    occurred_at_ms: prefill.occurred_at_ms ?? Date.now(),
    status: 'adopted', sources: prefill.sources ?? [], revisions: [], memories: [], tags: [],
  }).current;
  const initialWhen = useRef(localTime(base.occurred_at_ms)).current;
  const [fields, setFields] = useState(() => ({
    title: base.title, summary: base.summary, when: initialWhen, status: base.status,
    sources: base.sources.join(', '), revisions: base.revisions.join(', '),
    memories: base.memories.join(', '),
    tags: base.tags.join(', '), supersedes: base.supersedes ?? '',
  }));
  const [frozenId, setFrozenId] = useState(null);
  const idRef = useRef(null);
  function update(key, event) {
    const value = event.currentTarget.value;
    setFields(previous => ({ ...previous, [key]: value }));
  }
  async function run() {
    const title = fields.title.trim();
    const occurred = fields.when === initialWhen ? base.occurred_at_ms : new Date(fields.when).getTime();
    const args = {
      entry: entry ? entry.entry : idRef.current ?? `${slug(title)}-${Date.now().toString(36)}`,
      title, summary: fields.summary.trim(), occurred_at_ms: occurred, status: fields.status,
      sources: split(fields.sources), revisions: split(fields.revisions), memories: split(fields.memories),
      tags: split(fields.tags),
    };
    if (fields.supersedes.trim()) args.supersedes = fields.supersedes.trim();
    if (!title || bytes(title) > 200) throw new Error('The title is required and must be at most 200 bytes.');
    if (!args.summary) throw new Error('The summary is required.');
    if (!Number.isFinite(occurred)) throw new Error('Choose when the decision occurred.');
    if (args.sources.length < 1 || args.sources.length > 32) {
      throw new Error('Give between 1 and 32 source IDs.');
    }
    if (args.revisions.length > 32 || args.memories.length > 32) {
      throw new Error('At most 32 revisions and 32 memories.');
    }
    if (args.tags.length > 16 || args.tags.some(tag => !TAG_PATTERN.test(tag))) {
      throw new Error('At most 16 tags, each matching a-z, 0-9, ".", "_" or "-" ' +
        '(starting with a letter or digit).');
    }
    if (args.supersedes === args.entry) throw new Error('An entry cannot supersede itself.');
    if (entry) {
      if (JSON.stringify(args) === JSON.stringify(entryArgs(entry))) throw new Error('No changes to save.');
      args.expected_version = entry.version;
    } else {
      idRef.current = args.entry;
      setFrozenId(args.entry);
    }
    return api.tool(project, 'memory_timeline_write', args);
  }
  const body = html`<div class="timeline-fields">
    <p>${entry ? 'Save appends a new version without rewriting earlier history. ' +
      'If another edit has changed this entry, reload before editing again.' :
      'Record a curated decision backed by sources from this project. ' +
      'This does not approve memories or change their recall state.'}</p>
    ${prefill.truncated && html`<p>The source preview was truncated. Review the summary before saving.</p>`}
    <label>Title — at most 200 bytes
      <input value=${fields.title} maxlength="200" required onInput=${e => update('title', e)}/></label>
    <label>Summary <textarea value=${fields.summary} required
      onInput=${e => update('summary', e)}></textarea></label>
    <label>Occurred at <input type="datetime-local" value=${fields.when} required
      onInput=${e => update('when', e)}/></label>
    <label>Status <select value=${fields.status} onChange=${e => update('status', e)}>
      ${STATUSES.map(value => html`<option value=${value}>${value}</option>`)}
    </select></label>
    <p class="muted">${MEANINGS[fields.status]}</p>
    <label>Source evidence — required, exact source IDs from this project, comma-separated
      <input value=${fields.sources} required onInput=${e => update('sources', e)}/></label>
    <details><summary>Advanced links, tags and identifiers</summary>
      <p class="muted">${entry ? `Entry ${entry.entry} · editing version ${entry.version}` : frozenId
        ? `Entry ID ${frozenId} (kept if you retry)` :
        'Entry ID: assigned from the title on the first save, then kept for retries.'}</p>
      <label>Revisions — optional, comma-separated
        <input value=${fields.revisions} onInput=${e => update('revisions', e)}/></label>
      <label>Memories — optional, comma-separated
        <input value=${fields.memories} onInput=${e => update('memories', e)}/></label>
      <label>Supersedes entry — optional entry ID
        <input value=${fields.supersedes} onInput=${e => update('supersedes', e)}/></label>
      <label>Tags — optional, lowercase, comma-separated
        <input value=${fields.tags} onInput=${e => update('tags', e)}/></label>
    </details>
  </div>`;
  return html`<${ActionDialog} open=${true} title=${entry ? 'Edit timeline entry' : 'New timeline entry'}
    confirmLabel=${entry ? 'Save new version' : 'Create entry'} body=${body} run=${run}
    onDone=${receipt => onDone(receipt.entry)} onClose=${onClose}/>`;
}

function EntryButtons({ project, entry, onDone }) {
  const api = useContext(ApiContext);
  const [editing, setEditing] = useState(null);
  const [retract, setRetract] = useState(null);
  function snapshot() {
    return { project, entry: { ...entryArgs(entry), version: entry.version } };
  }
  async function run() {
    return api.tool(retract.project, 'memory_timeline_write', {
      ...entryArgs(retract.entry), status: 'retracted', expected_version: retract.entry.version,
    });
  }
  return html`<div class="timeline-actions">
    <button title="Save changes as a new version; keep previous history"
      onClick=${() => setEditing(snapshot())}>
      Edit entry</button>
    ${entry.status !== 'retracted' && html`<button class="danger"
      title="Append a retracted version; keep history and source coverage"
      onClick=${() => setRetract(snapshot())}>
      Retract</button>`}
    ${editing && html`<${EntryDialog} key=${JSON.stringify([editing.project, editing.entry.entry])}
      project=${editing.project} entry=${editing.entry} onDone=${onDone} onClose=${() => setEditing(null)}/>`}
    <${ActionDialog} open=${Boolean(retract)} title=${`Retract “${retract?.entry.title ?? ''}”?`}
      confirmLabel="Retract" danger=${true}
      body=${html`<p>This writes version ${(retract?.entry.version ?? 0) + 1} with status “retracted”.
        The history is kept and the entry still covers its sources.</p>`} run=${run} onDone=${onDone}
      onClose=${() => setRetract(null)}/>
  </div>`;
}

function Uncovered({ project, decisions, truncated, onCreate, refresh }) {
  if (!decisions.length) return null;
  function create(decision) {
    const preview = (decision.preview ?? '').trim();
    const first = preview.split('\n')[0].replace(/[\u0000-\u001f\u007f-\u009f]/g, ' ').trim();
    const title = bytes(first) <= 200 ? first : `${clip(first, 197)}…`;
    onCreate({ sources: [decision.source], title, summary: preview,
      occurred_at_ms: decision.recorded_at_ms, truncated: decision.preview_truncated });
  }
  return html`<details class="timeline-panel" open=${prefs.uncovered}
    onToggle=${e => { prefs.uncovered = e.currentTarget.open; }}>
    <summary>Captured decisions not yet on the timeline (${decisions.length}${truncated ? '+' : ''})</summary>
    <p>These captured decisions are not cited by any timeline entry${truncated ? ' (newest 16 shown)' : ''}.
      This is a separate curated record, not an automatic truth-approval queue.</p>
    ${decisions.map(decision => html`<article class="timeline-panel">
      <div class="timeline-heading"><${Badge}>${decision.outcome}</${Badge}>
        <span>${fmtTime(decision.recorded_at_ms)}</span></div>
      <p class="timeline-content">${decision.preview ?? ''}${decision.preview_truncated ? ' …' : ''}</p>
      <details><summary>Source provenance</summary><${Pairs} rows=${[
        ['Source', html`<code>${decision.source}</code>`], ['Origin', decision.origin ?? '—'],
      ]}/></details>
      <button class="primary" onClick=${() => create(decision)}>Record decision from this source</button>
      <details><summary>Permanent source deletion</summary>
        <p>Deletes this evidence and all dependent revisions and timeline entries. This cannot be undone.</p>
        <${PurgeAction} project=${project} source=${decision.source} onDone=${refresh}/>
      </details>
    </article>`)}
  </details>`;
}

function TimelineList({ project }) {
  const mounted = useRef(true);
  useEffect(() => () => { mounted.current = false; }, []);
  const api = useContext(ApiContext);
  const [order, setOrder] = useState(prefs.order);
  const [statuses, setStatuses] = useState(prefs.statuses);
  const [generation, setGeneration] = useState(0);
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const [busy, setBusy] = useState(false);
  const [create, setCreate] = useState(null);
  const sequence = useRef(0);
  function refresh() { setGeneration(value => value + 1); }
  function load() {
    const id = ++sequence.current;
    setError(null);
    setData(null);
    const args = { limit: 32, order };
    if (statuses.length) args.statuses = statuses;
    api.tool(project, 'memory_timeline', args).then(page => {
      if (id === sequence.current) setData(page);
    }).catch(err => {
      if (id === sequence.current) setError(err);
    });
    return () => { sequence.current++; };
  }
  useEffect(load, [project, order, statuses, generation]);
  async function more() {
    if (!data?.next_cursor || busy) return;
    const id = sequence.current;
    const args = { limit: 32, order, cursor: data.next_cursor };
    if (statuses.length) args.statuses = statuses;
    setBusy(true);
    try {
      const page = await api.tool(project, 'memory_timeline', args);
      if (id === sequence.current) setData(previous => ({ ...previous, ...page,
        entries: [...(previous.entries ?? []), ...(page.entries ?? [])],
        uncovered_decisions: previous.uncovered_decisions, uncovered_truncated: previous.uncovered_truncated,
      }));
    } catch (err) {
      if (id === sequence.current) setError(err);
    } finally { setBusy(false); }
  }
  function toggleStatus(status, checked) {
    const next = checked ? [...statuses, status] : statuses.filter(value => value !== status);
    prefs.statuses = next;
    setStatuses(next);
  }
  function toggleOrder() {
    const next = order === 'asc' ? 'desc' : 'asc';
    prefs.order = next;
    setOrder(next);
  }
  // A late response after leaving this view (or project) must not navigate away from where the user is.
  function created(entry, openedProject) {
    if (mounted.current && openedProject === project) location.hash = entryHref(openedProject, entry.entry);
  }
  if (error?.kind === 'needs_start') return html`<${NeedsStart} project=${project} onStarted=${refresh}/>`;
  const entries = data?.entries ?? [];
  const known = new Map(entries.map(entry => [entry.entry, entry]));
  return html`
    <div class="timeline-heading page-heading"><div><h1>Decision timeline</h1>
      <p>A curated history of this project’s decisions, separate from automatic memory processing.</p></div>
      <button class="primary" onClick=${() => setCreate({ project, prefill: {} })}>
        Record decision</button></div>
    <div class="timeline-filters"><button aria-pressed=${String(order === 'desc')} onClick=${toggleOrder}>
      Order: ${order === 'asc' ? 'oldest first' : 'newest first'}</button>
      <fieldset><legend>Status</legend>${STATUSES.map(status => html`<label>
        <input type="checkbox" checked=${statuses.includes(status)}
          onChange=${event => toggleStatus(status, event.currentTarget.checked)}/>
        <${Badge}>${status}</${Badge}></label>`)}</fieldset></div>
    ${error && html`<${ErrorNote} error=${error}/>`}
    ${!data && !error && html`<${Loading}/>`}
    ${data && html`
      <div class="timeline-list">
        ${entries.length ? entries.map(entry => html`<article class="timeline-panel">
          <div class="timeline-heading"><span>${fmtTime(entry.occurred_at_ms)}</span>
            <span><${Badge}>${entry.status}</${Badge}>
              <span class="muted">v${entry.version}</span></span></div>
          <h2><a href=${entryHref(project, entry.entry)}>${entry.title}</a></h2>
          <p class="timeline-content">${clip(entry.summary, 480)}</p>
          <p class="muted">Summary preview ·
            <a href=${entryHref(project, entry.entry)}>Read full decision</a></p>
          <p class="muted">${MEANINGS[entry.status]}</p><${Tags} tags=${entry.tags}/>
          ${entry.supersedes && html`<p>Supersedes ${known.has(entry.supersedes) || data.next_cursor ||
            statuses.length ? html`<a href=${entryHref(project, entry.supersedes)}>
              ${known.get(entry.supersedes)?.title ?? entry.supersedes}</a>` :
            html`<span>${entry.supersedes} (unavailable)</span>`}</p>`}
          <details><summary>Source links and technical details</summary>
            <${Pairs} rows=${[['Entry', entry.entry], ['Recorded', fmtTime(entry.recorded_at_ms)]]}/>
            <p>${(entry.sources ?? []).length} source(s). Open the entry to read all versions and evidence
              links.</p>
            <a href=${entryHref(project, entry.entry)}>Read entry and version history</a>
          </details>
          <${EntryButtons} project=${project} entry=${entry} onDone=${refresh}/>
        </article>`) : html`<${Empty} title=${statuses.length ? 'No entries with these statuses.' :
          'No timeline entries yet.'}/>`}
      </div>
      ${data.next_cursor && html`<button disabled=${busy} onClick=${more}>Load more</button>`}
      <${Uncovered} project=${project} decisions=${data.uncovered_decisions ?? []}
        truncated=${data.uncovered_truncated} onCreate=${prefill => setCreate({ project, prefill })}
        refresh=${refresh}/>
    `}
    ${create && html`<${EntryDialog} key=${JSON.stringify([create.project, create.prefill.sources ?? []])}
      project=${create.project} prefill=${create.prefill} onClose=${() => setCreate(null)}
      onDone=${entry => created(entry, create.project)}/>`}
  `;
}

function EntryDetail({ project, id }) {
  const api = useContext(ApiContext);
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const [target, setTarget] = useState(null);
  const [generation, setGeneration] = useState(0);
  function refresh() { setGeneration(value => value + 1); }
  function load() {
    let alive = true;
    setData(null);
    setError(null);
    setTarget(null);
    async function fetchData() {
      try {
        const versions = [];
        let cursor;
        let complete = false;
        for (let page = 0; page < 20; page++) {
          if (!alive) return;
          const args = { entry: id, limit: 32 };
          if (cursor) args.cursor = cursor;
          const result = await api.tool(project, 'memory_timeline', args);
          versions.push(...(result.entries ?? []));
          cursor = result.next_cursor;
          if (!cursor) { complete = true; break; }
        }
        if (!alive) return;
        setData({ versions, complete });
        const supersedes = versions.at(-1)?.supersedes;
        if (supersedes) {
          try {
            const result = await api.tool(project, 'memory_timeline', { entry: supersedes, limit: 1 });
            if (alive) setTarget({ found: Boolean(result.entries?.length) });
          } catch (err) {
            if (alive) setTarget({ error: errorText(err) });
          }
        }
      } catch (err) {
        if (alive) setError(err);
      }
    }
    fetchData();
    return () => { alive = false; };
  }
  useEffect(load, [project, id, generation]);
  if (error?.kind === 'needs_start') return html`<${NeedsStart} project=${project} onStarted=${refresh}/>`;
  if (error) return html`<${ErrorNote} error=${error}/>`;
  if (!data) return html`<${Loading}/>`;
  const { versions, complete } = data;
  const head = versions.at(-1);
  if (!head) return html`<p><a href=${href({ project, page: 'timeline' })}>← Timeline</a></p>
    <${Empty} title=${`${id} (unavailable)`}>
      This entry does not exist in the project, or it was deleted by a purge.</${Empty}>`;
  const supersedes = !head.supersedes ? 'none' : target?.found
    ? html`<a href=${entryHref(project, head.supersedes)}><code>${head.supersedes}</code></a>`
    : html`<span><code>${head.supersedes}</code>${target?.error ? ` (could not check: ${target.error})` :
      target && !target.found ? ' (unavailable)' : ''}</span>`;
  return html`
    <p><a href=${href({ project, page: 'timeline' })}>← Timeline</a></p>
    <div class="timeline-heading page-heading"><h1>${head.title}</h1><${Badge}>${head.status}</${Badge}></div>
    ${!complete && html`<p>History is incomplete. Editing is disabled until every version can be loaded.</p>`}
    <div class="timeline-detail"><div class="timeline-list">
      <article class="timeline-panel">
        <p class="muted">${complete ? `Current version · ${head.version}` :
          `Latest loaded version · ${head.version} · history incomplete`}</p>
        <p class="muted">Occurred ${fmtTime(head.occurred_at_ms)}</p>
        <p class="timeline-content">${head.summary}</p><${Tags} tags=${head.tags}/>
      </article>
      <details class="timeline-panel"><summary>Source evidence and memory links</summary>
        <p>Exact links to this project’s evidence. Curation does not certify the sources as truth or change
          memory lifecycle states.</p>
        <${Pairs} rows=${[['Sources', html`<${Links} project=${project} ids=${head.sources}/>`],
          ['Revisions', html`<${Links} project=${project} ids=${head.revisions}/>`],
          ['Memories', html`<${Links} project=${project} ids=${head.memories} page="memory"/>`]]}/>
      </details>
      <details class="timeline-panel"><summary>Technical identifiers</summary>
        <${Pairs} rows=${[['Entry', head.entry], ['Recorded', fmtTime(head.recorded_at_ms)],
          ['Supersedes', supersedes]]}/>
      </details>
      <details class="timeline-panel">
        <summary>Version history (${versions.length}${complete ? '' : '+'})</summary>
        <p>Chronological versions, including the displayed version. Earlier text and evidence links remain
          unchanged.</p>
        ${versions.map(version => html`<details class="timeline-version">
          <summary>Version ${version.version} · ${version.status} ·
            ${fmtTime(version.recorded_at_ms)}</summary>
          <h2>${version.title}</h2><p>Occurred ${fmtTime(version.occurred_at_ms)}</p>
          <p class="timeline-content">${version.summary}</p><p>${MEANINGS[version.status]}</p>
          <details><summary>Version provenance and identifiers</summary>
            <${Pairs} rows=${[['Entry', version.entry], ['Recorded', fmtTime(version.recorded_at_ms)],
              ['Supersedes', version.supersedes ? html`<a href=${entryHref(project, version.supersedes)}>
                <code>${version.supersedes}</code></a>` : '—'],
              ['Sources', html`<${Links} project=${project} ids=${version.sources}/>`],
              ['Revisions', html`<${Links} project=${project} ids=${version.revisions}/>`],
              ['Memories', html`<${Links} project=${project} ids=${version.memories} page="memory"/>`],
              ['Tags', (version.tags ?? []).join(', ') || '—']]}/>
          </details>
        </details>`)}
      </details>
    </div><aside class="timeline-list" aria-label="Decision context">
      <section class="timeline-panel"><h2>Decision state</h2><${Badge}>${head.status}</${Badge}>
        <p>${MEANINGS[head.status]}</p></section>
      ${complete && html`<section class="timeline-panel"><h2>Maintain this entry</h2>
        <p>Editing saves a new version. Retracting withdraws the decision but keeps its history and source
          coverage. Neither action deletes evidence.</p>
        <${EntryButtons} project=${project} entry=${head} onDone=${refresh}/>
      </section>`}
    </aside></div>
  `;
}

export function TimelineView({ project, route }) {
  return route.page === 'entry' ? html`<${EntryDetail} project=${project} id=${route.id}/>` :
    html`<${TimelineList} project=${project}/>`;
}

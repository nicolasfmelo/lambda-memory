import { html, useContext, useEffect, useRef, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { ApiContext } from '../components/nav.mjs';
import { Badge } from '../components/badge.mjs';
import { ActionDialog } from '../components/dialog.mjs';
import { Empty, ErrorNote, Loading, NeedsStart } from '../components/states.mjs';
import { errorText } from '../lib/api.mjs';
import { clip, fmtTime } from '../lib/format.mjs';
import { href } from '../lib/router.mjs';

const STATES = ['active', 'processing', 'conflicted', 'rejected', 'superseded'];
const MEANINGS = {
  active: 'Available for recall. Historical evidence, not verified truth.',
  processing: 'Automatic preparation is unfinished; this is not an approval queue.',
  conflicted: 'Conflicting revisions need resolution before they can be recalled.',
  rejected: 'Retired from review and excluded from recall; source evidence is retained.',
  superseded: 'Replaced by a newer active revision; retained as history.',
};

function Rows({ rows }) {
  return html`<dl class="review-rows">${rows.map(([label, value]) => html`
    <div><dt>${label}</dt><dd>${value ?? '—'}</dd></div>
  `)}</dl>`;
}

export function PurgeAction({ project, source, onDone }) {
  const api = useContext(ApiContext);
  const [dialog, setDialog] = useState(null);
  async function run() {
    const result = await api.api('/api/purge', {
      project: dialog.project, sources: [dialog.source], confirm: dialog.source,
    });
    if (result?.isError) throw new Error(errorText(result.body));
    return result;
  }
  return html`
    <button type="button" class="danger" onClick=${() => setDialog({ project, source })}>Purge source</button>
    <${ActionDialog} open=${Boolean(dialog)} onClose=${() => setDialog(null)} onDone=${onDone}
      title="Purge source" confirmLabel="Purge permanently" danger=${true} typed=${dialog?.source} run=${run}
      body=${html`
        <p>Source <code>${dialog?.source}</code></p>
        <p>This cannot be undone. Purging deletes the source and, in cascade, every revision derived from it
          and every timeline entry (with all its versions) that cites the source or a removed revision.</p>
      `}/>
  `;
}

function ReviewQueue({ project, state }) {
  const api = useContext(ApiContext);
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const [busy, setBusy] = useState(false);
  const [generation, setGeneration] = useState(0);
  const version = useRef(0);
  function loadFirst() {
    const mine = ++version.current;
    setData(null);
    setError(null);
    const args = { states: [state], limit: 32 };
    if (state === 'active') args.exclude_tree = true;
    api.tool(project, 'memory_list', args).then(page => {
      if (mine === version.current) setData(page);
    }).catch(err => {
      if (mine === version.current) setError(err);
    });
    return () => { version.current++; };
  }
  useEffect(loadFirst, [project, state, generation]);
  async function more() {
    if (!data?.next_cursor || busy) return;
    const mine = version.current;
    setBusy(true);
    try {
      const args = { states: [state], limit: 32, cursor: data.next_cursor };
      if (state === 'active') args.exclude_tree = true;
      const page = await api.tool(project, 'memory_list', args);
      if (mine === version.current) {
        setData({ ...page, revisions: [...(data.revisions ?? []), ...(page.revisions ?? [])] });
      }
    } catch (err) {
      if (mine === version.current) setError(err);
    } finally {
      setBusy(false);
    }
  }
  if (error?.kind === 'needs_start') {
    return html`<${NeedsStart} project=${project} onStarted=${() => setGeneration(generation + 1)}/>`;
  }
  return html`
    <div class="page-heading"><h1>Memories</h1>
      <p>Read source-backed content, inspect its provenance, and make deliberate corrections.</p></div>
    <nav class="review-tabs" aria-label="Lifecycle state">
      ${STATES.map(value => html`<a href=${href({ project, page: 'review', state: value })}
        aria-current=${state === value ? 'page' : undefined}>${value}</a>`)}
    </nav>
    <p class="muted">${MEANINGS[state]}</p>
    ${error && html`<${ErrorNote} error=${error}/>`}
    ${!data && !error && html`<${Loading}/>`}
    ${data && html`
      <div class="review-list">
        ${(data.revisions ?? []).length ? data.revisions.map(item => html`
          <article class="review-card">
            <div class="review-heading"><${Badge}>${item.state}</${Badge}>
              <span class="muted">Updated ${fmtTime(item.watermark_ms)}</span></div>
            <h2><a href=${href({ project, page: 'flat-memory', id: item.memory })}>
              ${clip(item.content ?? '', 240) || 'Open memory'}</a></h2>
            <p class="muted">${(item.sources ?? []).length} source(s) · content excerpt</p>
            <details><summary>Technical identifiers</summary>
              <${Rows} rows=${[['Memory', item.memory], ['Revision', item.revision]]}/>
            </details>
          </article>
        `) : html`<${Empty} title=${`No ${state} revisions.`}/>`}
      </div>
      ${data.truncated && !data.next_cursor && html`
        <p class="muted">Results truncated by the response budget.</p>`}
      ${data.next_cursor && html`<button disabled=${busy} onClick=${more}>Load more</button>`}
    `}
  `;
}

function RevisionCard({ project, item, items, active, jump, refresh }) {
  const api = useContext(ApiContext);
  const [action, setAction] = useState(null);
  const others = items.filter(other => other.revision !== item.revision &&
    ['active', 'processing', 'conflicted'].includes(other.state));
  const [choice, setChoice] = useState(active?.revision ?? others[0]?.revision ?? '');
  const open = item.state === 'processing' || item.state === 'conflicted';
  const predecessor = items.find(other => other.revision === item.predecessor);
  const audit = item.audit ?? [];
  function openAction(type) {
    setChoice(active?.revision ?? others[0]?.revision ?? '');
    setAction({ type, project, revision: item.revision, active: active?.revision, others });
  }
  async function run() {
    const args = { revision: action.revision, action: action.type };
    if (action.type === 'promote' && action.active) args.expected_active = action.active;
    if (action.type === 'conflict') args.conflict_with = choice;
    return api.tool(action.project, 'memory_review', args);
  }
  let body;
  if (action?.type === 'promote') {
    body = html`<p>This revision becomes active. ${action.active
      ? html`The current active revision <code>${action.active}</code> is checked again before replacement
        and becomes superseded.` : 'There is no active revision.'}</p>
      <p>Resolving a conflict also rejects the counterpart. Sources and history remain stored. Preparation
        may make billable model calls in OpenRouter mode; availability does not certify the claim as
        true.</p>`;
  } else if (action?.type === 'conflict') {
    body = html`<p>Both revisions become conflicted and unavailable for recall until the conflict is resolved.
      Sources and history remain stored.</p>
      <label>Counterpart revision <select value=${choice} onChange=${e => setChoice(e.currentTarget.value)}>
        ${action.others.map(other => html`<option value=${other.revision}>
          ${other.revision} (${other.state})</option>`)}
      </select></label>`;
  } else {
    body = html`<p>This revision stays out of recall and will no longer be prepared. Its sources and revision
      history remain stored. An active revision cannot be rejected.</p>`;
  }
  return html`
    <article class="review-card" data-revision=${item.revision} tabindex="-1">
      <div class="review-heading"><h2>Content</h2><${Badge}>${item.state}</${Badge}></div>
      <p class="review-content">${item.content ?? ''}</p>
      <p class="muted">${MEANINGS[item.state]}</p><p class="muted">Updated ${fmtTime(item.watermark_ms)}</p>
      ${open && html`<details><summary>Lifecycle actions</summary>
        <p>Promotion makes this revision active and supersedes the current active revision. Declaring a
          conflict marks both selected revisions as conflicted. Rejection keeps source evidence but excludes
          this revision from recall.</p>
        <div class="review-actions">
          <button onClick=${() => openAction('promote')}>
            ${item.state === 'conflicted' ? 'Resolve (promote)' : 'Promote'}</button>
          ${others.length > 0 && html`
            <button onClick=${() => openAction('conflict')}>Declare conflict</button>`}
          <button class="danger" onClick=${() => openAction('reject')}>Reject</button>
        </div>
      </details>`}
      <details><summary>Source provenance (${(item.sources ?? []).length})</summary>
        <p>Original source kind and outcome are preserved. Identifiers below are exact; provenance is not a
          truth guarantee.</p>
        ${(item.sources ?? []).map(source => html`<section class="review-source">
          <${Badge}>${source.kind}</${Badge}> <${Badge}>${source.outcome}</${Badge}>
          <${Rows} rows=${[['Origin', source.origin ?? '—'], ['Captured', fmtTime(source.recorded_at_ms)],
            ['Source', html`<code>${source.source}</code>`]]}/>
          <p>Permanent deletion also removes derived revisions and timeline entries that depend on this
            source.</p>
          <${PurgeAction} project=${project} source=${source.source} onDone=${refresh}/>
        </section>`)}
      </details>
      <details><summary>Technical identifiers and predecessor</summary>
        <${Rows} rows=${[['Memory', item.memory], ['Revision', item.revision],
          ['Predecessor', item.predecessor ? predecessor
            ? html`<button onClick=${() => jump(item.predecessor)}>${item.predecessor}</button>`
            : `${item.predecessor} (unavailable)` : 'none'],
          ['Reviewed', fmtTime(item.reviewed_at_ms)]]}/>
      </details>
      <details><summary>Lifecycle audit (${audit.length})</summary>
        ${audit.length ? html`<div class="review-table"><table>
          <thead><tr>${['When', 'Action', 'Transition', 'Expected active', 'Conflict with'].map(label =>
            html`<th scope="col">${label}</th>`)}</tr></thead>
          <tbody>${audit.map(row => html`<tr><td>${fmtTime(row.at_ms)}</td><td>${row.action}</td>
            <td>${row.from_state} → ${row.to_state}</td><td>${row.expected_active ?? '—'}</td>
            <td>${row.conflict_with ?? '—'}</td></tr>`)}</tbody>
        </table></div>` : html`<p>No lifecycle reviews recorded.</p>`}
      </details>
      <${ActionDialog} open=${Boolean(action)} onClose=${() => setAction(null)} onDone=${refresh}
        title=${action?.type === 'conflict' ? `Declare a conflict for ${action.revision}`
          : `${action?.type === 'promote' ? 'Promote' : 'Reject'} ${action?.revision ?? ''}?`}
        confirmLabel=${action?.type === 'conflict' ? 'Declare conflict' :
          action?.type === 'promote' ? 'Promote' : 'Reject'}
        danger=${action?.type === 'reject'} body=${body} run=${run}/>
    </article>
  `;
}

function CorrectCard({ project, memory, active, refresh }) {
  const api = useContext(ApiContext);
  const [content, setContent] = useState(active.content ?? '');
  const [dialog, setDialog] = useState(null);
  const attempt = useRef({ content: null, nonce: null });
  async function run() {
    const text = dialog.content.trim();
    if (!text) throw new Error('The corrected content is empty.');
    if (text === dialog.original.trim()) throw new Error('The content is unchanged.');
    if (attempt.current.content !== text) attempt.current = { content: text, nonce: crypto.randomUUID() };
    const out = await api.api('/api/correct', {
      project: dialog.project, memory: dialog.memory, predecessor: dialog.revision,
      content: text, nonce: attempt.current.nonce,
    });
    if (out?.capture?.isError) throw new Error(`Capture failed: ${errorText(out.capture.body)}`);
    if (out?.propose?.isError) {
      throw new Error(`The source was captured, but the proposal failed: ${errorText(out.propose.body)}`);
    }
    return out;
  }
  return html`<details class="review-card"><summary>Correct this memory</summary>
    <p>Preserves the existing evidence, records a manual-curation decision source and proposes a new revision
      whose predecessor is the active revision <code>${active.revision}</code>.</p>
    <label>Corrected content <textarea value=${content} onInput=${e => setContent(e.currentTarget.value)}
      required></textarea></label>
    <button class="primary" onClick=${() => setDialog({
      project, memory, revision: active.revision, original: active.content ?? '', content,
    })}>Propose correction</button>
    <${ActionDialog} open=${Boolean(dialog)} title="Propose correction?" confirmLabel="Propose correction"
      body=${html`<p>Preserves the existing evidence, records a manual-curation decision source and proposes
        a new revision whose predecessor is the active revision <code>${dialog?.revision}</code>.</p>`}
      run=${run} onDone=${refresh} onClose=${() => setDialog(null)}/>
  </details>`;
}

function MemoryDetail({ project, memory }) {
  const api = useContext(ApiContext);
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const [generation, setGeneration] = useState(0);
  function refresh() { setGeneration(value => value + 1); }
  function load() {
    let alive = true;
    setData(null);
    setError(null);
    async function fetchData() {
      try {
        const items = [];
        let cursor;
        let complete = false;
        const flat = await api.tool(project, 'memory_list', {
          memory, states: ['active'], exclude_tree: true,
        });
        for (let page = 0; page < 20; page++) {
          if (!alive) return;
          const args = { memory, states: STATES, limit: 32 };
          if (cursor) args.cursor = cursor;
          const result = await api.tool(project, 'memory_list', args);
          items.push(...(result.revisions ?? []));
          cursor = result.next_cursor;
          if (!cursor) { complete = true; break; }
        }
        if (alive) setData({ items, complete, flat: flat.revisions ?? [] });
      } catch (err) {
        if (alive) setError(err);
      }
    }
    fetchData();
    return () => { alive = false; };
  }
  useEffect(load, [project, memory, generation]);
  function jump(revision) {
    const element = [...document.querySelectorAll('[data-revision]')]
      .find(node => node.dataset.revision === revision);
    if (!element) return;
    for (let node = element.parentElement; node; node = node.parentElement) {
      if (node.tagName === 'DETAILS') node.open = true;
    }
    element.scrollIntoView({ block: 'start' });
    element.focus({ preventScroll: true });
  }
  if (error?.kind === 'needs_start') return html`<${NeedsStart} project=${project} onStarted=${refresh}/>`;
  if (error) return html`<${ErrorNote} error=${error}/>`;
  if (!data) return html`<${Loading}/>`;
  const { items, complete, flat } = data;
  const active = items.find(item => item.state === 'active');
  const correct = active && flat.some(item => item.memory === memory && item.revision === active.revision);
  const sorted = [...items].sort((a, b) => (b.watermark_ms ?? 0) - (a.watermark_ms ?? 0));
  const featured = active ?? sorted[0];
  const byId = new Map(items.map(item => [item.revision, item]));
  const succeeded = new Set(items.map(item => item.predecessor).filter(Boolean));
  const chains = items.filter(item => !succeeded.has(item.revision)).map(head => {
    const chain = [];
    const seen = new Set();
    for (let node = head; node && !seen.has(node.revision); node = byId.get(node.predecessor)) {
      seen.add(node.revision);
      chain.unshift(node);
    }
    return chain;
  }).sort((a, b) => Number(b.some(n => n.state === 'active')) - Number(a.some(n => n.state === 'active'))
    || b.length - a.length);
  return html`
    <p><a href=${href({ project, page: 'review' })}>← Memories</a></p><h1>Memory detail</h1>
    ${!complete && html`<p>Showing the first revisions only. This memory has more revisions than the UI loads
      at once.</p>`}
    ${!featured ? html`<${Empty} title="No revisions found for this memory."/>` : html`
      <div class="review-detail">
        <div class="review-main">
          <p class="muted">${active ? 'Current active revision' :
            'Newest loaded revision · no active revision is available'}</p>
          ${!active && html`<p>This is not active recall content. Read the lifecycle state before using it.
            Only processing or conflicted revisions can be promoted.</p>`}
          <${RevisionCard} project=${project} item=${featured} items=${items} active=${active}
            jump=${jump} refresh=${refresh}/>
          ${correct && html`<${CorrectCard} key=${JSON.stringify([project, memory, active.revision])}
            project=${project} memory=${memory}
            active=${active} refresh=${refresh}/>`}
          <details class="review-card"><summary>Other revisions (${items.length - 1})</summary>
            <p>Past and nonactive revisions remain available for comparison. Each revision keeps its own
              evidence and lifecycle audit.</p>
            ${sorted.filter(item => item !== featured).map(item => html`<details class="review-history">
              <summary>${item.state} · ${fmtTime(item.watermark_ms)} · ${item.revision}</summary>
              <${RevisionCard} project=${project} item=${item} items=${items} active=${active}
                jump=${jump} refresh=${refresh}/>
            </details>`)}
          </details>
          <details class="review-card"><summary>Revision lineage</summary>
            ${chains.map(chain => html`<div class="review-lineage">
              ${chain[0]?.predecessor && !byId.has(chain[0].predecessor) &&
                html`<span>${chain[0].predecessor} (unavailable) →</span>`}
              ${chain.map((node, index) => html`<span>${index ? ' → ' : ''}<button
                onClick=${() => jump(node.revision)}>${node.revision} · ${node.state}</button></span>`)}
            </div>`)}
          </details>
        </div>
        <aside class="review-main" aria-label="Memory context">
          <section class="review-card"><h2>Recall state</h2><${Badge}>${featured.state}</${Badge}>
            <p>${MEANINGS[featured.state]}</p></section>
          <section class="review-card"><h2>Source evidence</h2>
            <p>${(featured.sources ?? []).length} source(s) support this revision.</p>
            <p>Sources preserve the original event and outcome. Successful processing does not turn a failed
              or unknown event into verified truth. Expand source provenance below the content for exact
              evidence identifiers.</p></section>
          <details class="review-card"><summary>Memory identifiers</summary>
            <${Rows} rows=${[['Project', project], ['Memory', memory],
              ['Displayed revision', featured.revision]]}/></details>
        </aside>
      </div>`}
  `;
}

export function ReviewView({ project, route }) {
  if (route.page === 'flat-memory') return html`<${MemoryDetail} project=${project} memory=${route.id}/>`;
  const state = STATES.includes(route.state) ? route.state : 'active';
  return html`<${ReviewQueue} project=${project} state=${state}/>`;
}

import { html, useContext, useEffect, useRef, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { ApiContext } from '../components/nav.mjs';
import { Badge } from '../components/badge.mjs';
import { ErrorNote, Loading, NeedsStart } from '../components/states.mjs';
import { fmtTime } from '../lib/format.mjs';
import { href } from '../lib/router.mjs';

const operations = ['capture', 'propose', 'dream', 'list', 'recall', 'review', 'purge', 'timeline'];
const outcomes = ['running', 'succeeded', 'failed', 'indeterminate'];
const meanings = {
  active: 'Available for recall. Historical evidence, not verified truth.',
  processing: 'Automatic preparation is unfinished; this is not an approval queue.',
  conflicted: 'Conflicting revisions need resolution before they can be recalled.',
  rejected: 'Retired from review and excluded from recall; source evidence is retained.',
  superseded: 'Replaced by a newer active revision; retained as history.',
};
const duration = ms => ms == null ? '—' : ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;

function Pairs({ rows }) {
  return html`<dl class="activity-pairs">
    ${rows.map(([label, value]) => html`<div><dt>${label}</dt><dd>${value}</dd></div>`)}
  </dl>`;
}

function StatusCards({ status, project }) {
  const pending = status.pending ?? {};
  const worker = status.worker ?? {};
  const dream = worker.dreaming ?? {};
  const diagnostics = status.diagnostics ?? {};
  const last = [
    ['Last success', status.last_success],
    ['Last failure', status.last_failure],
    ['Last dream success', status.last_dream_success],
    ['Last dream failure', status.last_dream_failure],
  ];
  return html`
    <section class="activity-panel">
      <h2>Memories</h2>
      <p class="muted">Mode <code>${status.mode}</code> · observed ${fmtTime(status.observed_at_ms)}</p>
      <div class="activity-states">
        ${Object.entries(meanings).map(([state, meaning]) => html`
          <a href=${href({ project, page: 'review', state })}>
            <strong>${status.lifecycle?.[state] ?? 0}</strong>
            <${Badge}>${state}</${Badge}>
            <span>${meaning}</span>
          </a>
        `)}
      </div>
    </section>
    <section class="activity-panel">
      <h2>Pending work</h2>
      <p class="muted">
        Automatic preparation, not a human approval queue. Blocked work needs investigation;
        this screen never retries it.
      </p>
      <${Pairs} rows=${[
        ['Unconsumed sources', pending.unconsumed_sources ?? 0],
        ['Processing revisions', pending.processing_revisions ?? 0],
        ['Blocked sources', pending.blocked_sources ?? 0],
        ['Blocked revisions', pending.blocked_revisions ?? 0],
        ['Active without current embedding', pending.active_without_current_embedding ?? 0],
        ['Unresolved conflicts', status.review?.unresolved_conflicts ?? 0],
      ]}/>
    </section>
    <details class="activity-panel">
      <summary>Worker diagnostics</summary>
      <${Pairs} rows=${[
        ['Instance', html`<code>${worker.instance ?? '—'}</code>`],
        ['Started', fmtTime(worker.started_at_ms)],
        ['Foreground active', worker.foreground_active ? 'yes' : 'no'],
        ['Dreaming', html`<span><${Badge}>${dream.state ?? '—'}</${Badge}>
          ${dream.phase && html`<code> ${dream.phase}</code>`}
          ${dream.stop_reason && ` stopped: ${dream.stop_reason}`}</span>`],
        ['Idle threshold', duration(dream.idle_ms)],
        ['Remaining idle', duration(dream.remaining_idle_ms)],
        ['Tick / batch', `${duration(dream.tick_ms)} / ${dream.dream_batch ?? '—'}`],
        ['Recording', html`<span><${Badge}>${diagnostics.state ?? '—'}</${Badge}>
          ${diagnostics.reason && html`<code> ${diagnostics.reason}</code>`}</span>`],
        ['History', `${status.history?.retention_days ?? '—'} days retained since ` +
          fmtTime(status.history?.retained_since_ms)],
      ]}/>
    </details>
    <details class="activity-panel">
      <summary>Recent operation outcomes</summary>
      <div class="activity-table"><table>
        <thead><tr><th></th><th>Operation</th><th>Outcome</th><th>Reason</th><th>Finished</th></tr></thead>
        <tbody>${last.map(([label, entry]) => html`<tr>
          <th scope="row">${label}</th>
          ${entry ? html`
            <td><code>${entry.operation}/${entry.phase}</code></td>
            <td><${Badge}>${entry.outcome}</${Badge}></td>
            <td><code>${entry.reason || '—'}</code></td>
            <td>${fmtTime(entry.finished_at_ms ?? entry.started_at_ms)}</td>
          ` : html`<td colspan="4">none</td>`}
        </tr>`)}</tbody>
      </table></div>
    </details>
  `;
}

function ActivityTable({ entries }) {
  return html`<div class="activity-table"><table>
    <thead><tr>${['Started', 'Operation', 'Phase', 'Kind', 'Outcome', 'Reason', 'Duration', 'Counts']
      .map(label => html`<th scope="col">${label}</th>`)}</tr></thead>
    <tbody>
      ${entries.length ? entries.map(entry => html`<tr>
        <td>${fmtTime(entry.started_at_ms)}</td>
        <td><code>${entry.operation}</code></td>
        <td><code>${entry.phase}</code></td>
        <td>${entry.entry_kind}</td>
        <td><${Badge}>${entry.outcome}</${Badge}></td>
        <td><code>${entry.reason || '—'}</code></td>
        <td>${duration(entry.duration_ms)}</td>
        <td>${Object.entries(entry.counts ?? {}).map(([key, value]) =>
          `${key.replace(/_/g, ' ')}: ${value}`).join(', ') || '—'}</td>
      </tr>`) : html`<tr><td colspan="8">No activity recorded.</td></tr>`}
    </tbody>
  </table></div>`;
}

export function ActivityView({ project }) {
  const api = useContext(ApiContext);
  const [status, setStatus] = useState(null);
  const [entries, setEntries] = useState(null);
  const [cursor, setCursor] = useState(null);
  const [operation, setOperation] = useState('');
  const [outcome, setOutcome] = useState('');
  const [error, setError] = useState(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [generation, setGeneration] = useState(0);
  const pages = useRef(1);
  const paging = useRef(false);
  const current = useRef(0);

  function load() {
    const id = ++current.current;
    let busy = false;
    let stopped = false;
    pages.current = 1;
    setStatus(null);
    setEntries(null);
    setCursor(null);
    setError(null);
    async function poll() {
      if (busy || stopped || document.hidden) return;
      busy = true;
      try {
        const args = { limit: 20 };
        if (operation) args.operations = [operation];
        if (outcome) args.outcomes = [outcome];
        const refreshActivity = pages.current <= 1 && !paging.current;
        const [nextStatus, activity] = await Promise.all([
          api.tool(project, 'memory_status', {}),
          refreshActivity ? api.tool(project, 'memory_activity', args) : Promise.resolve(null),
        ]);
        if (id !== current.current) return;
        setStatus(nextStatus);
        if (activity && pages.current <= 1 && !paging.current) {
          setEntries(activity.entries ?? []);
          setCursor(activity.next_cursor ?? null);
        }
      } catch (err) {
        if (id === current.current) {
          setError(err);
          stopped = true;
        }
      } finally {
        busy = false;
      }
    }
    poll();
    const timer = setInterval(poll, 10000);
    document.addEventListener('visibilitychange', poll);
    return () => {
      current.current++;
      clearInterval(timer);
      document.removeEventListener('visibilitychange', poll);
    };
  }
  useEffect(load, [project, operation, outcome, generation]);

  async function more() {
    if (!cursor || paging.current) return;
    const id = current.current;
    paging.current = true;
    setLoadingMore(true);
    try {
      const args = { limit: 20, cursor };
      if (operation) args.operations = [operation];
      if (outcome) args.outcomes = [outcome];
      const page = await api.tool(project, 'memory_activity', args);
      if (id !== current.current) return;
      setEntries(previous => [...previous, ...(page.entries ?? [])]);
      setCursor(page.next_cursor ?? null);
      pages.current++;
    } catch (err) {
      if (id === current.current) setError(err);
    } finally {
      paging.current = false;
      setLoadingMore(false);
    }
  }
  if (error?.kind === 'needs_start') {
    return html`<${NeedsStart} project=${project} onStarted=${() => setGeneration(generation + 1)}/>`;
  }
  return html`
    <div class="page-heading">
      <h1>Activity & diagnostics</h1>
      <p>Recall readiness and processing for this project. Active memories remain provisional evidence,
        not verified truth.</p>
    </div>
    ${error && html`<${ErrorNote} error=${error}/>`}
    ${error && html`<button onClick=${() => setGeneration(generation + 1)}>Reload</button>`}
    ${!status || !entries ? !error && html`<${Loading}/>` : html`
      <div class="activity-layout">
        <${StatusCards} status=${status} project=${project}/>
        <section class="activity-panel">
          <h2>Activity</h2>
          <p class="muted">Read-only processing history. These observations do not start work or retry
            failed operations.</p>
          <div class="activity-filters">
            <label>Operation <select value=${operation} onChange=${event => setOperation(event.target.value)}>
              <option value="">All</option>
              ${operations.map(value => html`<option value=${value}>${value}</option>`)}
            </select></label>
            <label>Outcome <select value=${outcome} onChange=${event => setOutcome(event.target.value)}>
              <option value="">All</option>
              ${outcomes.map(value => html`<option value=${value}>${value}</option>`)}
            </select></label>
          </div>
          <${ActivityTable} entries=${entries}/>
          ${cursor && html`<button onClick=${more} disabled=${loadingMore}>Load more</button>`}
          <p class="muted">Auto-refresh every 10 s while visible.</p>
        </section>
      </div>
    `}
  `;
}

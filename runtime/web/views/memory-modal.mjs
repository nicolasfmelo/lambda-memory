import { html, useContext, useEffect, useRef, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { ApiContext, NeighborsContext } from '../components/nav.mjs';
import { Modal, ActionDialog } from '../components/dialog.mjs';
import { Badge, BasisBadge } from '../components/badge.mjs';
import { Empty, ErrorNote, Loading, NeedsStart } from '../components/states.mjs';
import { errorText } from '../lib/api.mjs';
import { fmtDay, fmtTime } from '../lib/format.mjs';
import { href } from '../lib/router.mjs';
import { neighbor } from '../lib/grid-state.mjs';
import { plural } from '../lib/plural.mjs';

export function MemoryModal({ project, node, onClose, onOpen }) {
  const api = useContext(ApiContext);
  const { ids } = useContext(NeighborsContext);
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const [tab, setTab] = useState('Content');
  const [dialog, setDialog] = useState(null);
  const [text, setText] = useState('');
  const [loadingMore, setLoadingMore] = useState(false);
  const [generation, setGeneration] = useState(0);
  const nonce = useRef(null);
  const sequence = useRef(0);
  const previous = neighbor(ids, node, -1);
  const next = neighbor(ids, node, 1);

  function fetchNode() {
    const token = ++sequence.current;
    setData(null);
    setError(null);
    setTab('Content');
    setDialog(null);
    setLoadingMore(false);
    nonce.current = null;
    api.tool(project, 'memory_node', { node, history: true, sources: true }).then(result => {
      if (token === sequence.current) setData(result);
    }).catch(err => {
      if (token === sequence.current) setError(err);
    });
    return () => { sequence.current++; };
  }

  function keys() {
    function keydown(event) {
      if (dialog || event.defaultPrevented || /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)) return;
      const target = event.key === 'ArrowLeft' ? previous : event.key === 'ArrowRight' ? next : null;
      if (target) {
        event.preventDefault();
        onOpen(target);
      }
    }
    document.addEventListener('keydown', keydown);
    return () => document.removeEventListener('keydown', keydown);
  }

  useEffect(fetchNode, [project, node, generation]);
  useEffect(keys, [dialog, previous, next, onOpen]);

  async function moreSources() {
    const token = sequence.current;
    setLoadingMore(true);
    try {
      const page = await api.tool(project, 'memory_node', {
        node, history: true, sources: true, cursor: data.next_cursor,
      });
      if (token !== sequence.current) return;
      setData(current => {
        const seen = new Set(current.sources.map(source => source.source));
        const sources = [...current.sources];
        for (const source of page.sources || []) {
          if (!seen.has(source.source)) {
            seen.add(source.source);
            sources.push(source);
          }
        }
        return { ...current, sources, next_cursor: page.next_cursor };
      });
    } catch (err) {
      if (token === sequence.current) setError(err);
    } finally {
      if (token === sequence.current) setLoadingMore(false);
    }
  }

  function correction() {
    setText(data.content || '');
    setDialog({ type: 'correction' });
  }

  async function sendCorrection() {
    if (!nonce.current || nonce.current.text !== text) {
      nonce.current = { text, value: crypto.randomUUID(), title: data.title, path: data.path || [] };
    }
    const { value, title, path } = nonce.current;
    const result = await api.api('/api/correction', {
      project, node, content: text, nonce: value, title, path,
    });
    if (result.capture?.isError) throw new Error(`Capture failed: ${errorText(result.capture.body)}`);
    return result;
  }

  async function purge() {
    const result = await api.api('/api/purge', { project, sources: [dialog.source], confirm: dialog.source });
    if (result?.isError) throw new Error(errorText(result.body));
    return result;
  }

  function reload() {
    setGeneration(value => value + 1);
  }

  const missing = error?.kind === 'tool' && error.body?.error === 'record not found';
  const tabs = data && [
    ['Content', null], ['Sources', data.source_total], ['History', (data.history || []).length],
    ['Links', (data.links || []).length],
  ];
  const footer = data && html`
    <div class="memory-footer">
      <button class="primary" onClick=${correction}>Send correction…</button>
      <div class="memory-neighbors">
        <button aria-label="Previous memory" disabled=${!previous} onClick=${() => onOpen(previous)}>
          ‹
        </button>
        <button aria-label="Next memory" disabled=${!next} onClick=${() => onOpen(next)}>›</button>
      </div>
    </div>
  `;
  return html`
    <${Modal} open=${true} title=${data?.title || 'Memory'} onClose=${onClose} wide=${true} footer=${footer}>
      ${missing && html`<${Empty} title="Not in the tree">
        <a class="memory-link" href=${href({ project, page: 'flat-memory', id: node })}>Open flat memory</a>
      </${Empty}>`}
      ${error && !missing && (error.kind === 'needs_start'
        ? html`<${NeedsStart} project=${project} onStarted=${reload}/>`
        : html`<${ErrorNote} error=${error}/>`)}
      ${!data && !error && html`<${Loading}/>`}
      ${data && html`
        <div class="memory-path">${(data.path || []).join(' › ')}</div>
        <div class="memory-badges">
          <${BasisBadge} basis=${data.basis} confidence=${data.confidence}/>
          ${data.valid_from_ms != null && html`
            <${Badge}>valid since ${fmtDay(data.valid_from_ms)}</${Badge}>
          `}
          ${data.replaced && html`
            <${Badge} tone="updated">updated ${fmtDay(data.replaced.valid_until_ms)}</${Badge}>
          `}
          <${Badge}>${plural(data.source_total, 'source')}</${Badge}>
          <${Badge}>${plural((data.links || []).length, 'link')}</${Badge}>
          ${data.skill_draft?.status === 'drafted' && html`
            <${Badge} tone="draft">skill draft ready</${Badge}>
          `}
        </div>
        <div class="memory-tabs" role="tablist" aria-label="Memory details">
          ${tabs.map(([name, count]) => html`
            <button role="tab" aria-selected=${tab === name} onClick=${() => setTab(name)}>
              ${name}${count == null ? '' : ` (${count})`}
            </button>
          `)}
        </div>
        <section class="memory-tab-content" role="tabpanel">
          ${tab === 'Content' && html`
            <div class="memory-text">${data.content}</div>
            ${data.lesson && html`<p class="memory-lesson">
              reinforced in ${plural(data.lesson.reinforced_sessions, 'session')} ·
              ${plural(data.lesson.strategies, 'strategy', 'strategies')} ·
              ${plural(data.lesson.pitfalls, 'pitfall')}
            </p>`}
            ${data.basis === 'concluded' && html`
              <div class="memory-derived">
                <strong>from ${plural((data.derived_from || []).length, 'memory', 'memories')}</strong>
                ${(data.derived_memories || []).map(item => html`
                  <button class="memory-link" onClick=${() => onOpen(item.node)}>${item.title}</button>
                `)}
                <details><summary>Revision identifiers</summary>
                  ${(data.derived_from || []).map(id => html`<div><code>${id}</code></div>`)}
                </details>
              </div>
            `}
          `}
          ${tab === 'Sources' && html`
            ${(data.sources || []).length ? data.sources.map(source => html`
              <article class="memory-record">
                <div class="record-meta">
                  ${source.kind} · ${source.operation} · ${fmtTime(source.recorded_at_ms)}
                </div>
                <div class="memory-text">${source.content}</div>
                <div class="record-meta">${source.origin} · <code>${source.source}</code></div>
                <button onClick=${() => setDialog({ type: 'purge', source: source.source })}>
                  Purge source
                </button>
              </article>
            `) : html`<p class="muted">No sources linked.</p>`}
            ${data.next_cursor && html`<button disabled=${loadingMore} onClick=${moreSources}>
              ${loadingMore ? 'Loading…' : 'Load more sources'}
            </button>`}
          `}
          ${tab === 'History' && html`
            ${(data.history || []).length ? data.history.map(item => html`
              <article class="memory-record">
                <div class="record-meta">
                  ${fmtTime(item.valid_from_ms)} – ${fmtTime(item.valid_until_ms)}
                </div>
                <div class="memory-text">${item.content}</div><code>${item.revision}</code>
              </article>
            `) : html`<p class="muted">No history recorded.</p>`}
          `}
          ${tab === 'Links' && html`
            ${(data.links || []).length ? data.links.map(item => html`
              <div class="memory-record">
                <div class="record-meta">
                  ${item.relation} · ${item.direction === 'out' ? 'outgoing' : 'incoming'}
                </div>
                <button class="memory-link" onClick=${() => onOpen(item.node)}>${item.title}</button>
              </div>
            `) : html`<p class="muted">No linked memories.</p>`}
          `}
        </section>
      `}
    </${Modal}>
    <${ActionDialog} open=${dialog?.type === 'correction'} title="Send correction"
      confirmLabel="Send correction"
      onClose=${() => setDialog(null)} run=${sendCorrection} body=${html`
        <p>This is sent as a new source; curation applies it to this memory shortly.
          In OpenRouter mode curation is a billable call.</p>
        <label>Corrected content<textarea rows="8" value=${text}
          onInput=${event => setText(event.currentTarget.value)}></textarea></label>
      `}/>
    <${ActionDialog} open=${dialog?.type === 'purge'} title="Purge source" confirmLabel="Purge permanently"
      danger=${true} typed=${dialog?.source} run=${purge} onDone=${reload} onClose=${() => setDialog(null)}
      body=${html`
        <p>Source <code>${dialog?.source}</code></p>
        <p>This cannot be undone. Purging deletes the source and, in cascade, every revision derived from it
          and every timeline entry (with all its versions) that cites the source or a removed revision.</p>
      `}/>
  `;
}

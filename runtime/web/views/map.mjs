import { html, useContext, useEffect, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { ApiContext } from '../components/nav.mjs';
import { Empty, ErrorNote, Loading, NeedsStart } from '../components/states.mjs';
import { squarify } from '../lib/treemap.mjs';
import { href } from '../lib/router.mjs';
import { fmtTime } from '../lib/format.mjs';
import { needsYou, chartData } from '../lib/map.mjs';

// Renders the memory dashboard for project and route.
export function MapView({ project, route }) {
  const api = useContext(ApiContext);
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const [generation, setGeneration] = useState(0);

  function pollOverview() {
    let alive = true;
    let inFlight = false;
    let stopped = false;

    async function load() {
      if (!alive || inFlight || stopped || document.hidden) {
        return;
      }
      inFlight = true;
      try {
        const [overview, status] = await Promise.all([
          api.tool(project, 'memory_overview', { utc_offset_minutes: -new Date().getTimezoneOffset() }),
          api.tool(project, 'memory_status'),
        ]);
        if (alive) {
          setData({ overview, status });
          setError(null);
        }
      } catch (e) {
        if (alive) {
          setError(e);
          stopped = true;
        }
      } finally {
        inFlight = false;
      }
    }

    function visible() {
      if (!document.hidden) {
        load();
      }
    }

    setData(null);
    setError(null);
    load();
    const timer = setInterval(load, 10000);
    document.addEventListener('visibilitychange', visible);
    return function stopPolling() {
      alive = false;
      clearInterval(timer);
      document.removeEventListener('visibilitychange', visible);
    };
  }

  function refreshAfterStart() {
    setGeneration(generation + 1);
  }

  useEffect(pollOverview, [project, generation]);
  if (error?.kind === 'needs_start') {
    return html`<${NeedsStart} project=${project} onStarted=${refreshAfterStart}/>`;
  }
  if (error) {
    return html`<${ErrorNote} error=${error}/>`;
  }
  if (!data) {
    return html`<${Loading}/>`;
  }

  const { overview: o, status: s } = data;
  const roots = o.mode === 'flat' ? [] : o.roots || [];
  const tiles = roots.map(r => ({
    key: r.root,
    label: r.label,
    value: r.nodes,
    latest: r.latest?.title,
    page: 'type',
    id: r.root,
  }));
  if (o.mode === 'flat' || o.flat_active > 0) {
    tiles.push({
      key: 'flat',
      label: o.mode === 'flat' ? 'Not yet in the tree' : 'Outside the tree',
      value: o.flat_active || 0,
      latest: o.mode === 'flat'
        ? 'Tree migration organizes these memories.'
        : 'Active memories outside the tree.',
      page: 'flat',
      id: o.mode === 'flat' ? 'unsorted' : 'outside',
    });
  }
  // Empty types sit in a compact row under the map: squeezed into the treemap they become unreadable slivers.
  const filled = tiles.filter(t => t.value > 0);
  const empty = tiles.filter(t => !t.value);
  const rects = squarify(filled, 1000, 650);
  const chart = chartData(o.days || []);
  const chartLabel = `${chart.captured} captured and ${chart.published} published in 30 days`;

  function renderTile(t, i) {
    const r = rects[i];
    const position = {
      left: `${r.x / 10}%`,
      top: `${r.y / 6.5}%`,
      width: `${r.w / 10}%`,
      height: `${r.h / 6.5}%`,
    };
    return html`
      <a class=${`map-tile root-${t.key} ${t.value ? '' : 'tile-empty'}`}
        href=${href({ project, page: t.page, id: t.id })}
        aria-label=${`${t.label}: ${t.value} memories`} style=${position}>
        <strong>${t.label}</strong>
        <span class="tile-title">${t.latest || (t.value ? '' : 'Empty')}</span>
        <b>${t.value}</b>
      </a>
    `;
  }

  function renderEmptyType(t) {
    return html`
      <a class=${`empty-type type-${t.key}`} href=${href({ project, page: t.page, id: t.id })}
        aria-label=${`${t.label}: no memories yet`}>
        <span class="empty-dot" aria-hidden="true"></span>${t.label}<span class="empty-note">empty</span>
      </a>
    `;
  }

  function renderQueue(row) {
    return html`
      <a class="rail-row queue-link" href=${href({ project, ...row.route })}>
        <span>${row.label}</span>
        <span class="queue-end">
          <strong>${row.count}</strong>
          <span class="chevron" aria-hidden="true">›</span>
        </span>
      </a>
    `;
  }

  function renderDay(d) {
    return html`
      <div class="chart-day" title=${d.tooltip}>
        <i class="bar-captured" style=${{ height: `${d.capturedHeight}%` }}></i>
        <i class="bar-published" style=${{ height: `${d.publishedHeight}%` }}></i>
      </div>
    `;
  }

  return html`
    <div class="page-heading">
      <div>
        <h1>Memory map</h1>
        <p>Active memories are available for recall. Provisional evidence, not verified truth.</p>
      </div>
    </div>
    ${tiles.every(t => !t.value) && html`
      <${Empty} title="No active memories yet">Captured work will appear here when it is published.</${Empty}>
    `}
    <div class="dashboard">
      <section aria-label="Memory types">
        ${filled.length > 0 && html`<div class="treemap">${filled.map(renderTile)}</div>`}
        ${empty.length > 0 && html`
          <div class="empty-types" aria-label="Types without memories yet">${empty.map(renderEmptyType)}</div>
        `}
      </section>
      <aside class="rail">
        <section class="panel">
          <h2>Needs you</h2>
          ${needsYou(o, s).map(renderQueue)}
        </section>
        <section class="panel">
          <h2>Last 30 days</h2>
          <p class="chart-summary">${chart.captured} captured · ${chart.published} published in 30 days</p>
          <div class="chart-layout" aria-label=${chartLabel}>
            <span class="chart-max">${chart.max}</span>
            <div class="chart-plot">
              <div class="chart">${chart.days.map(renderDay)}</div>
              <div class="chart-axis">
                <span>${chart.days[0]?.label || ''}</span>
                <span>${chart.days.at(-1)?.label || ''}</span>
              </div>
            </div>
          </div>
          <div class="chart-key">
            <span><i class="swatch captured"></i>Captured</span>
            <span><i class="swatch published"></i>Published</span>
          </div>
        </section>
        <section class="panel">
          <h2>Health</h2>
          <div class="rail-row">
            <span>Worker</span>
            <strong>Live</strong>
          </div>
          <div class="rail-row">
            <span>Without current embedding</span>
            <strong>${s.pending?.active_without_current_embedding ?? '—'}</strong>
          </div>
          <div class="rail-row">
            <span>Forwards pending / failed</span>
            <strong>${s.forwards_pending ?? 0} / ${s.forwards_failed ?? 0}</strong>
          </div>
          <div class="rail-row">
            <span>Last dream success</span>
            <span>${fmtTime(s.last_dream_success?.finished_at_ms)}</span>
          </div>
          <div class="rail-row">
            <span>Last dream failure</span>
            <span>${fmtTime(s.last_dream_failure?.finished_at_ms)}</span>
          </div>
        </section>
      </aside>
    </div>
  `;
}

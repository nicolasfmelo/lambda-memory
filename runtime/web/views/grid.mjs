import { html, useContext, useEffect, useRef, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { ApiContext, NeighborsContext } from '../components/nav.mjs';
import { Badge, BasisBadge } from '../components/badge.mjs';
import { Empty, ErrorNote, Loading, NeedsStart } from '../components/states.mjs';
import { href } from '../lib/router.mjs';
import { fmtDay } from '../lib/format.mjs';
import { debounceKey, flatCount, gridReducer, initialGrid } from '../lib/grid-state.mjs';
import { plural } from '../lib/plural.mjs';

const labels = {
  decisions: 'Decisions', preferences: 'Preferences', architecture: 'Architecture',
  open_items: 'Open items', lessons: 'Lessons', working_rules: 'Working rules',
  general_lessons: 'General lessons',
};

function Lesson({ lesson, date }) {
  const sessions = lesson.sessions ?? lesson.reinforced_sessions ?? 0;
  return html`
    <div class="lesson-reinforcement">
      <div class="lesson-pips" aria-label=${plural(sessions, 'session')}>
        ${Array.from({ length: 5 }, (_, index) => html`
          <i class=${index < Math.min(sessions, 5) ? 'filled' : ''}></i>
        `)}
      </div>
      <span>reinforced in ${plural(sessions, 'session')} · updated ${fmtDay(date)}</span>
    </div>
    <${Badge} tone="strategy">${plural(lesson.strategies, 'strategy', 'strategies')}</${Badge}>
    <${Badge} tone="pitfall">${plural(lesson.pitfalls, 'pitfall')}</${Badge}>
  `;
}

function Card({ item, project, flat }) {
  const id = flat ? item.memory : item.node;
  return html`
    <a class="memory-card" href=${href({ project, page: flat ? 'flat-memory' : 'memory', id })}>
      <div class="card-path">${flat ? 'Outside the tree' : (item.path || []).slice(0, -1).join(' › ')}</div>
      <h2>${item.title || item.memory}</h2>
      <p class="card-summary">${flat ? item.content : item.summary}</p>
      <div class="card-badges">
        ${!flat && html`<${BasisBadge} basis=${item.basis} confidence=${item.confidence}/>`}
        ${item.basis === 'concluded' && html`
          <${Badge}>from ${plural(item.derived_count, 'memory', 'memories')}</${Badge}>
        `}
        ${item.replaced && html`
          <${Badge} tone="updated">updated ${fmtDay(item.replaced.valid_until_ms)}</${Badge}>
        `}
        ${!flat && html`<${Badge}>${plural(item.source_count, 'source')}</${Badge}>`}
        ${!flat && html`<${Badge}>${plural(item.link_count, 'link')}</${Badge}>`}
        ${item.lesson && html`<${Lesson} lesson=${item.lesson} date=${item.updated_at_ms}/>`}
        ${item.skill_draft?.status === 'drafted' && html`<${Badge} tone="draft">skill draft ready</${Badge}>`}
      </div>
    </a>
  `;
}

export function GridView({ project, route }) {
  const api = useContext(ApiContext);
  const { setIds } = useContext(NeighborsContext);
  const routeKey = JSON.stringify([project, route.page, route.id]);
  const [state, setState] = useState(() => initialGrid(routeKey));
  const [search, setSearch] = useState('');
  const [meta, setMeta] = useState({ total: 0, topics: [] });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [generation, setGeneration] = useState(0);
  const serial = useRef(0);
  const flat = route.page === 'flat';
  const label = flat ? (route.id === 'unsorted' ? 'Not yet in the tree' : 'Outside the tree')
    : labels[route.id] || route.id;

  function change(key, value) {
    if (state.filters[key] === value) return;
    serial.current++;
    setLoading(true);
    setState(current => gridReducer(current, { type: 'filter', key, value }));
  }

  function searchDelay() {
    if (search === state.filters.query) return;
    const timer = setTimeout(() => change('query', search), 250);
    return () => clearTimeout(timer);
  }

  async function load(cursor, token) {
    try {
      const filters = state.filters;
      const args = flat ? { states: ['active'], exclude_tree: true, limit: 32 } : {
        root: route.id, sort: filters.sort, limit: 32,
        ...(filters.query ? { query: filters.query } : {}),
        ...(filters.topic ? { topic: filters.topic } : {}),
        ...(filters.basis ? { basis: filters.basis } : {}),
        ...(filters.updated ? { updated: true } : {}),
      };
      if (cursor) args.cursor = cursor;
      const result = await api.tool(project, flat ? 'memory_list' : 'memory_nodes', args);
      if (token !== serial.current) return;
      const nodes = flat ? (result.revisions || []).map(row => ({ ...row, node: row.memory }))
        : result.nodes || [];
      setState(current => gridReducer(current, { type: 'page', nodes, cursor: result.next_cursor }));
      setMeta({ total: result.total ?? 0, topics: result.topics || [] });
      setError(null);
    } catch (e) {
      if (token === serial.current) setError(e);
    } finally {
      if (token === serial.current) setLoading(false);
    }
  }

  function loadFirst() {
    const token = ++serial.current;
    setLoading(true);
    setError(null);
    load(null, token);
    return () => { serial.current++; };
  }

  function publishIds() {
    setIds(flat ? [] : state.cards.map(card => card.node));
    return () => setIds([]);
  }

  function resetRoute() {
    if (state.routeKey === routeKey) return;
    serial.current++;
    setSearch('');
    setState(current => gridReducer(current, { type: 'route', key: routeKey }));
    setMeta({ total: 0, topics: [] });
  }

  useEffect(resetRoute, [routeKey]);
  useEffect(searchDelay, [debounceKey(search), state.filters.query]);
  useEffect(loadFirst, [state.routeKey, state.filters, generation]);
  useEffect(publishIds, [state.cards, flat]);

  const selected = state.filters.updated ? 'updated' : state.filters.basis || 'all';
  function chooseFilter(value) {
    if (selected === value) return;
    serial.current++;
    setLoading(true);
    setState(current => gridReducer(current, { type: 'filters', values: {
      basis: value === 'observed' || value === 'concluded' ? value : '',
      updated: value === 'updated',
    } }));
  }
  function more() {
    setLoading(true);
    load(state.cursor, ++serial.current);
  }
  function started() {
    setGeneration(value => value + 1);
  }

  if (error?.kind === 'needs_start') return html`<${NeedsStart} project=${project} onStarted=${started}/>`;
  return html`
    <div class="page-heading grid-heading">
      <div class="grid-crumb">
        <a href=${href({ project, page: 'map' })}>Map</a> › ${label} · ${flat ? flatCount(state) : meta.total}
      </div>
      <h1>${label}</h1>
      <p>Active memories are available for recall. Provisional evidence, not verified truth.</p>
    </div>
    ${!flat && html`
      <div class="grid-controls">
        <input type="search" aria-label="Search memories" placeholder=${`Search in ${label}…`}
          value=${search} onInput=${event => setSearch(event.currentTarget.value)}/>
        <div class="grid-chips" aria-label="Filter memories">
          ${['all', 'observed', 'concluded', 'updated'].map(value => html`
            <button type="button" aria-pressed=${selected === value} onClick=${() => chooseFilter(value)}>
              ${value[0].toUpperCase() + value.slice(1)}
            </button>
          `)}
        </div>
        <select aria-label="Topic" value=${state.filters.topic}
          onChange=${event => change('topic', event.currentTarget.value)}>
          <option value="">All topics</option>
          ${meta.topics.map(topic => html`
            <option value=${topic.node}>${topic.title} (${topic.count})</option>
          `)}
        </select>
        <select aria-label="Sort" value=${state.filters.sort}
          onChange=${event => change('sort', event.currentTarget.value)}>
          <option value="recent">Recent</option><option value="sources">Most sources</option>
        </select>
      </div>
    `}
    ${error && html`<${ErrorNote} error=${error}/>`}
    ${state.cards.length > 0 && html`
      <div class="memory-grid">
        ${state.cards.map(item => html`<${Card} item=${item} project=${project} flat=${flat}/>`)}
      </div>
    `}
    ${loading && !state.cards.length && html`<${Loading}/>`}
    ${!loading && !error && !state.cards.length && html`
      <${Empty} title="No memories found">Try another filter or return to the map.</${Empty}>
    `}
    ${state.cursor && html`
      <div class="grid-more">
        <button disabled=${loading} onClick=${more}>${loading ? 'Loading…' : 'Load more'}</button>
      </div>
    `}
  `;
}

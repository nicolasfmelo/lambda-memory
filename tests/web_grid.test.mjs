import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { initialGrid, gridReducer, debounceKey, neighbor, flatCount } from '../runtime/web/lib/grid-state.mjs';
import { plural } from '../runtime/web/lib/plural.mjs';

test('each filter change resets cards and cursor', () => {
  const state = { ...initialGrid(), cards: [{ node: 'a' }], cursor: 'next' };
  for (const [key, value] of Object.entries({ query: 'hello', basis: 'observed', updated: true,
    topic: 'topic', sort: 'sources' })) {
    const next = gridReducer(state, { type: 'filter', key, value });
    assert.deepEqual(next.cards, []);
    assert.equal(next.cursor, null);
    assert.equal(next.filters[key], value);
  }
});

test('route reset preserves filter identity on mount and resets once for a new route', () => {
  const initial = initialGrid('demo/type/decisions');
  const same = gridReducer(initial, { type: 'route', key: 'demo/type/decisions' });
  assert.strictEqual(same, initial);
  assert.strictEqual(same.filters, initial.filters);
  const changed = gridReducer(initial, { type: 'route', key: 'demo/type/lessons' });
  assert.notStrictEqual(changed.filters, initial.filters);
  assert.strictEqual(gridReducer(changed, { type: 'route', key: 'demo/type/lessons' }), changed);
});

test('flat counts reflect all loaded cards and indicate a next page', () => {
  let state = gridReducer(initialGrid(), { type: 'page', nodes: [{ node: 'a' }, { node: 'b' }], cursor: 'x' });
  assert.equal(flatCount(state), '2+');
  state = gridReducer(state, { type: 'page', nodes: [{ node: 'b' }, { node: 'c' }], cursor: null });
  assert.equal(flatCount(state), '3');
});

test('count labels use singular and plural forms', () => {
  for (const [one, many] of [['link', 'links'], ['source', 'sources'], ['strategy', 'strategies'],
    ['pitfall', 'pitfalls'], ['memory', 'memories'], ['session', 'sessions']]) {
    assert.equal(plural(1, one, many), `1 ${one}`);
    assert.equal(plural(0, one, many), `0 ${many}`);
    assert.equal(plural(2, one, many), `2 ${many}`);
  }
});

test('pages append without duplicate node ids', () => {
  let state = gridReducer(initialGrid(), { type: 'page', nodes: [{ node: 'a' }], cursor: 'next' });
  state = gridReducer(state, { type: 'page', nodes: [{ node: 'a' }, { node: 'b' }], cursor: null });
  assert.deepEqual(state.cards.map(card => card.node), ['a', 'b']);
  assert.equal(state.cursor, null);
});

test('debounce key changes only when the search changes', () => {
  assert.equal(debounceKey('abc'), debounceKey('abc'));
  assert.notEqual(debounceKey('abc'), debounceKey('abcd'));
});

test('card and modal styles constrain long Unicode paths and text on narrow screens', async () => {
  const grid = await readFile('runtime/web/views/grid.css', 'utf8');
  const modal = await readFile('runtime/web/views/memory-modal.css', 'utf8');
  assert.match(grid, /minmax\(min\(100%,300px\),1fr\)/);
  assert.match(grid, /overflow-wrap:anywhere/);
  assert.match(grid, /text-overflow: ellipsis/);
  assert.match(modal, /overflow-x: hidden/);
  assert.match(modal, /overflow-wrap:anywhere/);
});

test('neighbors stop at ends and ignore missing nodes', () => {
  assert.equal(neighbor(['a', 'b'], 'a', -1), null);
  assert.equal(neighbor(['a', 'b'], 'a', 1), 'b');
  assert.equal(neighbor(['a', 'b'], 'b', -1), 'a');
  assert.equal(neighbor(['a', 'b'], 'b', 1), null);
  assert.equal(neighbor(['a', 'b'], 'missing', 1), null);
  assert.equal(neighbor([], 'a', 1), null);
});

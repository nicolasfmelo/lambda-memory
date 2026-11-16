import test from 'node:test';
import assert from 'node:assert/strict';
import { createProjectRegistry, projectTarget, listenForVisible } from '../runtime/web/lib/projects.mjs';

const deferred = () => {
  let resolve;
  let reject;
  const promise = new Promise((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
};

test('registry refresh publishes the latest result and older callers follow it', async () => {
  const first = deferred();
  const second = deferred();
  const calls = [first, second];
  const updates = [];
  const registry = createProjectRegistry({
    load: () => calls.shift().promise,
    onUpdate: update => updates.push(update),
  });
  const older = registry.refresh();
  const newer = registry.refresh();
  first.resolve({ projects: [{ project: 'old', live: false }] });
  await Promise.resolve();
  assert.equal(updates.length, 0);
  second.resolve({ projects: [{ project: 'new', live: true }] });
  assert.deepEqual(await older, await newer);
  assert.deepEqual(updates, [{ projects: [{ project: 'new', live: true }], error: null }]);
});

test('a late failed request cannot replace a newer successful registry', async () => {
  const first = deferred();
  const second = deferred();
  const calls = [first, second];
  const updates = [];
  const registry = createProjectRegistry({
    load: () => calls.shift().promise,
    onUpdate: update => updates.push(update),
  });
  const older = registry.refresh();
  const newer = registry.refresh();
  second.resolve({ projects: [{ project: 'new' }] });
  await newer;
  first.reject(new Error('stale failure'));
  assert.deepEqual(await older, { projects: [{ project: 'new' }], error: null });
  assert.equal(updates.length, 1);
});

test('registry errors are surfaced instead of confirming a missing project', async () => {
  const error = new Error('offline');
  const updates = [];
  const registry = createProjectRegistry({
    load: async () => { throw error; },
    onUpdate: update => updates.push(update),
  });
  assert.deepEqual(await registry.refresh(), { error });
  assert.deepEqual(updates, [{ error }]);
});

test('remembered project wins only while still registered', () => {
  const projects = [{ project: '@user' }, { project: 'one' }, { project: 'two' }];
  assert.equal(projectTarget(projects, 'two'), 'two');
  assert.equal(projectTarget(projects, 'removed'), 'one');
  assert.equal(projectTarget(projects, null), 'one');
  assert.equal(projectTarget(projects, '@user'), 'one');
  assert.equal(projectTarget([{ project: '@user' }], 'two'), undefined);
});

test('visibility refresh fires only when visible and stops after cleanup', () => {
  const doc = new EventTarget();
  let count = 0;
  doc.hidden = true;
  const cleanup = listenForVisible(doc, () => { count++; });
  doc.dispatchEvent(new Event('visibilitychange'));
  assert.equal(count, 0);
  doc.hidden = false;
  doc.dispatchEvent(new Event('visibilitychange'));
  assert.equal(count, 1);
  cleanup();
  doc.dispatchEvent(new Event('visibilitychange'));
  assert.equal(count, 1);
});

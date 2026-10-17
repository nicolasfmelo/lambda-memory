import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { loadConfig } from '../runtime/project-binding.mjs';
import { loadRegistry, recordProject } from '../runtime/project-registry.mjs';

function dir(t) {
  const root = mkdtempSync(join(tmpdir(), 'lm-reg-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

test('registry round-trips with mode 0600 and a missing file is empty', async (t) => {
  const root = dir(t);
  assert.deepEqual(loadRegistry(root), []);
  assert.equal(await recordProject(root, { project: 'p1', cwd: '/work/p1' }, 1000), true);
  assert.deepEqual(loadRegistry(root), [{ project: 'p1', cwd: '/work/p1', last_seen_ms: 1000 }]);
  assert.equal(statSync(join(root, 'projects.json')).mode & 0o777, 0o600);
});

test('registry keeps 256 entries and evicts the least recently seen', async (t) => {
  const root = dir(t);
  for (let i = 0; i < 258; i += 1) await recordProject(root, { project: `p${i}`, cwd: `/w/${i}` }, 1000 + i);
  const entries = loadRegistry(root);
  assert.equal(entries.length, 256);
  assert.ok(!entries.some((entry) => entry.project === 'p0' || entry.project === 'p1'));
  assert.equal(entries[0].project, 'p257');
});

test('repeat writes for the same project and cwd are debounced for 60s; a new cwd writes at once', async (t) => {
  const root = dir(t);
  await recordProject(root, { project: 'p', cwd: '/a' }, 1000);
  assert.equal(await recordProject(root, { project: 'p', cwd: '/a' }, 30000), false);
  assert.equal(loadRegistry(root)[0].last_seen_ms, 1000);
  assert.equal(await recordProject(root, { project: 'p', cwd: '/b' }, 31000), true);
  assert.deepEqual(loadRegistry(root), [{ project: 'p', cwd: '/b', last_seen_ms: 31000 }]);
  assert.equal(await recordProject(root, { project: 'p', cwd: '/b' }, 91001), true);
  assert.equal(loadRegistry(root)[0].last_seen_ms, 91001);
});

test('concurrent records are serialized without losing entries', async (t) => {
  const root = dir(t);
  await Promise.all(Array.from({ length: 20 }, (_, i) => recordProject(root, { project: `c${i}`, cwd: `/w/${i}` }, 5000 + i)));
  assert.equal(loadRegistry(root).length, 20);
});

test('loadRegistry rejects unsafe or corrupt files', (t) => {
  const root = dir(t), path = join(root, 'projects.json');
  const write = (content, mode = 0o600) => { writeFileSync(path, content, { mode }); chmodSync(path, mode); };
  write('{"projects":[]}', 0o644);
  assert.throws(() => loadRegistry(root));
  write('not json');
  assert.throws(() => loadRegistry(root));
  write(JSON.stringify({ projects: [{ project: 'p', cwd: 'relative', last_seen_ms: 1 }] }));
  assert.throws(() => loadRegistry(root));
  write(JSON.stringify({ projects: [{ project: 'p', cwd: '/a\nb', last_seen_ms: 1 }] }));
  assert.throws(() => loadRegistry(root));
  write(JSON.stringify({ projects: [{ project: 'x'.repeat(513), cwd: '/a', last_seen_ms: 1 }] }));
  assert.throws(() => loadRegistry(root));
  write(JSON.stringify({ projects: [], pad: 'x'.repeat(256 * 1024) }));
  assert.throws(() => loadRegistry(root));
});

test('a corrupt registry is rebuilt on the next record', async (t) => {
  const root = dir(t);
  writeFileSync(join(root, 'projects.json'), 'garbage', { mode: 0o600 });
  await recordProject(root, { project: 'p', cwd: '/a' }, 1);
  assert.equal(loadRegistry(root).length, 1);
});

function config(t, extra) {
  const root = dir(t), path = join(root, 'runtime.json'), bin = resolve('tests/support/fake-native.cjs');
  chmodSync(bin, 0o755);
  writeFileSync(path, JSON.stringify({ nativeBin: bin, bridgeBin: bin, dataDir: join(root, 'd'), socketPath: join(root, 's'), owner: 'o', actor: 'a', token: 'x'.repeat(40), ...extra }), { mode: 0o600 });
  return () => loadConfig({ LAMBDA_MEMORY_CONFIG: path });
}

test('loadConfig validates webPort and webLeaseMs', (t) => {
  const absent = config(t, {})();
  assert.equal(absent.webPort, undefined);
  assert.equal(absent.webLeaseMs, 120000);
  const ok = config(t, { webPort: 8765, webLeaseMs: 5000 })();
  assert.equal(ok.webPort, 8765); assert.equal(ok.webLeaseMs, 5000);
  for (const bad of [{ webPort: 1023 }, { webPort: 65536 }, { webPort: 8080.5 }, { webPort: '8080' }, { webPort: null }, { webLeaseMs: 4999 }, { webLeaseMs: 3600001 }, { webLeaseMs: '9000' }]) {
    assert.throws(config(t, bad), /invalid web configuration/, JSON.stringify(bad));
  }
  assert.equal(config(t, { webPort: 1024, webLeaseMs: 3600000 })().webPort, 1024);
  assert.equal(config(t, { webPort: 65535 })().webPort, 65535);
});

test('an invalid entry is skipped and leaves registered projects intact', async (t) => {
  const root = dir(t);
  await recordProject(root, { project: 'good', cwd: '/w/good' }, 1000);
  for (const bad of [{ project: 'bad', cwd: '/tmp/a\nb' }, { project: 'bad', cwd: `/${'x'.repeat(4096)}` }, { project: 'bad', cwd: 'rel' }, { project: '', cwd: '/a' }]) {
    assert.equal(await recordProject(root, bad, 2000), false);
  }
  assert.deepEqual(loadRegistry(root), [{ project: 'good', cwd: '/w/good', last_seen_ms: 1000 }]);
});

test('a stale or symlinked tmp file is replaced and no tmp is left behind', async (t) => {
  const root = dir(t), temp = join(root, `projects.json.${process.pid}.tmp`), victim = join(root, 'victim');
  writeFileSync(victim, 'keep');
  symlinkSync(victim, temp);
  await recordProject(root, { project: 'p', cwd: '/a' }, 1);
  assert.equal(readFileSync(victim, 'utf8'), 'keep');
  assert.equal(existsSync(temp), false);
  assert.equal(loadRegistry(root).length, 1);
});

test('a failed rename cleans the tmp and a transient read error does not wipe the file', async (t) => {
  const root = dir(t);
  await recordProject(root, { project: 'a', cwd: '/a' }, 1);
  // Make the destination a directory so rename fails.
  rmSync(join(root, 'projects.json'));
  mkdirSync(join(root, 'projects.json'));
  await assert.rejects(recordProject(root, { project: 'b', cwd: '/b' }, 2));
  assert.equal(existsSync(join(root, `projects.json.${process.pid}.tmp`)), false);
});

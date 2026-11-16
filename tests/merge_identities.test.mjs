import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

function fixture(t, options = {}) {
  const root = mkdtempSync(join(tmpdir(), 'lm-identity-')); t.after(() => rmSync(root, { recursive: true, force: true }));
  const data = join(root, 'data'), bin = join(root, 'bin'), ignored = join(root, 'ignored');
  for (const d of [data, bin, ignored]) mkdirSync(d);
  writeFileSync(join(bin, 'systemctl'), `#!/bin/sh\necho ${options.active ? 'active' : 'inactive'}\n`, { mode: 0o755 });
  const native = join(bin, 'native');
  const fake = resolve('tests/support/fake-operator.cjs');
  const configure = (opts = {}) => writeFileSync(native, `#!/usr/bin/env node\n${Object.entries(opts).map(([k,v]) => `process.env[${JSON.stringify(k)}]=${JSON.stringify(String(v))};`).join('\n')}\nrequire(${JSON.stringify(fake)});\n`, { mode: 0o755 });
  configure(options.fake);
  const config = join(root, 'runtime.json');
  writeFileSync(config, JSON.stringify({ nativeBin: native, bridgeBin: native, dataDir: data, socketPath: join(root, 's'), owner: 'o', actor: 'a', token: 'x'.repeat(32), ignoreRoots: [ignored] }), { mode: 0o600 });
  const registry = entries => writeFileSync(join(data, 'projects.json'), JSON.stringify({ projects: entries.map(e => ({ ...e, last_seen_ms: 1 })) }), { mode: 0o600 });
  const store = project => join(data, createHash('sha256').update(project).digest('hex'), 'store');
  const put = (project, rows) => { mkdirSync(store(project), { recursive: true }); writeFileSync(join(store(project), 'sources.jsonl'), rows.map(x => JSON.stringify(x)).join('\n') + (rows.length ? '\n' : '')); };
  const get = project => readFileSync(join(store(project), 'sources.jsonl'), 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
  let script = resolve('runtime/merge-identities.mjs');
  if (options.limits) {
    script = join(root, 'runner.mjs');
    writeFileSync(script, `import { main } from ${JSON.stringify(new URL(`file://${resolve('runtime/merge-identities.mjs')}`).href)};\nmain(${JSON.stringify(options.limits)}).catch(() => { process.exitCode = 2; });\n`);
  }
  const run = (args, input) => spawnSync(process.execPath, [script, ...args], { encoding: 'utf8', input, env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, LAMBDA_MEMORY_CONFIG: config } });
  const inspect = () => { const r = run(['inspect']); assert.equal(r.status, 0, r.stderr); return JSON.parse(r.stdout); };
  const apply = (plan, backup = join(root, 'backup')) => run(['apply', '--backup', backup], JSON.stringify({ ...plan, accept_charges: true }));
  const entries = () => JSON.parse(readFileSync(join(data, 'projects.json'), 'utf8')).projects;
  const marker = (name = 'R') => { const dir = join(root, name); mkdirSync(dir, { recursive: true }); writeFileSync(join(dir, 'CLAUDE.md'), ''); return dir; };
  return { root, data, ignored, registry, store, put, get, run, inspect, apply, entries, marker, configure };
}
const entry = cwd => ({ project: `path:${cwd}`, cwd });
const row = (source, content = source) => ({ source, origin: 'test://source', recorded_at_ms: 1, kind: 'exchange', outcome: 'unknown', content });
function sub(root, name) { const dir = join(root, name); mkdirSync(dir, { recursive: true }); return dir; }

test('plan merges subfolders into the marker root and drops ignored entries', t => {
  const f = fixture(t), r = f.marker(), a = sub(r, 'a'), b = sub(a, 'b'), x = sub(f.ignored, 'x');
  f.registry([entry(r), entry(a), entry(b), entry(x)]); f.put(`path:${a}`, [row('a')]);
  const p = f.inspect(); assert.deepEqual(p.merges.map(x => [x.from_project, x.to_project]), [[`path:${a}`, `path:${r}`], [`path:${b}`, `path:${r}`]]);
  assert.equal(p.merges[0].curatable, 1); assert.equal(p.estimated_curation_calls, 1); assert.deepEqual(p.drops, [entry(x)]); assert.equal(p.kept, 1);
});
test('plan merges same-folder path and repo pairs into repo', t => {
  const f = fixture(t), g = sub(f.root, 'git');
  assert.equal(spawnSync('git', ['init', '-q', g]).status, 0); assert.equal(spawnSync('git', ['-C', g, 'remote', 'add', 'origin', 'https://example.com/acme/app.git']).status, 0);
  f.registry([entry(g), { project: 'repo:example.com/acme/app', cwd: g }]); const p = f.inspect();
  assert.equal(p.merges.length, 1); assert.equal(p.merges[0].to_project, 'repo:example.com/acme/app'); assert.equal(p.kept, 1);
});
test('plan never self-merges and is stable across two inspects', t => {
  const f = fixture(t), r = f.marker(), a = sub(r, 'a'); f.registry([entry(r), entry(a)]);
  const first = f.run(['inspect']), second = f.run(['inspect']); assert.equal(first.status, 0, first.stderr); assert.equal(first.stdout, second.stdout);
  assert.ok(JSON.parse(first.stdout).merges.every(x => x.from_project !== x.to_project));
});
test('plan lists missing directories', t => { const f = fixture(t), d = join(f.root, 'gone'); f.registry([entry(d)]); assert.deepEqual(f.inspect().missing, [entry(d)]); });
test('apply refuses a changed plan or accept_charges false', t => {
  const f = fixture(t), r = f.marker(); f.registry([entry(r)]); const before = readFileSync(join(f.data, 'projects.json'), 'utf8'), p = f.inspect();
  for (const plan of [p, { ...p, kept: 999, accept_charges: true }]) { const result = f.run(['apply', '--backup', join(f.root, 'backup')], JSON.stringify(plan)); assert.equal(result.status, 2); assert.match(result.stderr, /exact identity merge consent required/); }
  assert.equal(readFileSync(join(f.data, 'projects.json'), 'utf8'), before); assert.equal(existsSync(join(f.root, 'backup')), false);
});
test('apply refuses when the service is active', t => { const f = fixture(t, { active: true }); const r = f.run(['apply', '--backup', join(f.root, 'backup')], '{}'); assert.equal(r.status, 2); assert.match(r.stderr, /service to be stopped/); });
test('apply exports, imports and removes merged and dropped entries from the registry', t => {
  const f = fixture(t), r = f.marker(), a = sub(r, 'a'), ignored = sub(f.ignored, 'x'), missing = join(f.root, 'gone'), kept = sub(f.root, 'kept');
  f.registry([entry(a), entry(ignored), entry(missing), entry(kept)]); f.put(`path:${a}`, [row('a')]); f.put(`path:${r}`, [row('old')]);
  const p = f.inspect(), result = f.apply(p); assert.equal(result.status, 0, result.stderr); const report = JSON.parse(result.stdout);
  assert.equal(report.outcomes[0].outcome, 'merged'); assert.equal(report.dropped, 1); assert.deepEqual(f.get(`path:${r}`).map(x => x.source), ['old', 'a']);
  assert.deepEqual(new Set(f.entries().map(x => x.project)), new Set([`path:${r}`, `path:${missing}`, `path:${kept}`]));
  assert.ok(existsSync(join(report.backup, 'projects.json'))); assert.equal(statSync(report.backup).mode & 0o777, 0o700); assert.ok(existsSync(join(report.backup, createHash('sha256').update(`path:${r}`).digest('hex'), 'store', 'sources.jsonl'))); assert.ok(existsSync(f.store(`path:${a}`)));
});
test('apply creates a private missing target store parent', t => {
  const f = fixture(t), r = f.marker(), a = sub(r, 'a');
  f.registry([entry(a)]); f.put(`path:${a}`, [row('s')]);
  const plan = f.inspect(), old = process.umask(0o022);
  try { const result = f.apply(plan); assert.equal(result.status, 0, result.stderr); }
  finally { process.umask(old); }
  assert.equal(statSync(dirname(f.store(`path:${r}`))).mode & 0o777, 0o700);
});
test('apply stops at the first failed merge and reports per-entry outcomes', t => {
  const f = fixture(t), roots = ['A', 'B', 'C'].map(n => f.marker(n)), dirs = roots.map(r => sub(r, 'a'));
  f.registry(dirs.map(entry)); dirs.forEach((d, i) => f.put(`path:${d}`, [row(`s${i}`)]));
  f.configure({ FAKE_OPERATOR_FAIL_IMPORT: `path:${roots[1]}` }); const first = f.apply(f.inspect()); assert.equal(first.status, 2, first.stderr);
  assert.deepEqual(JSON.parse(first.stdout).outcomes.map(x => x.outcome), ['merged', 'failed']);
  f.configure(); const second = f.apply(f.inspect(), join(f.root, 'backup2')); assert.equal(second.status, 0, second.stderr);
  assert.equal(JSON.parse(second.stdout).outcomes[0].duplicates, 1); assert.deepEqual(f.entries().map(x => x.project).sort(), roots.map(r => `path:${r}`).sort());
});
test('apply keeps a partial merge in the registry', t => {
  const f = fixture(t, { fake: { FAKE_OPERATOR_HEADROOM: 2 } }), r = f.marker(), a = sub(r, 'a'); f.registry([entry(a)]); f.put(`path:${a}`, Array.from({ length: 5 }, (_, i) => row(`s${i}`)));
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr); const o = JSON.parse(result.stdout).outcomes[0]; assert.equal(o.outcome, 'partial'); assert.equal(o.remaining, 3);
  assert.deepEqual(new Set(f.entries().map(x => x.project)), new Set([`path:${a}`, `path:${r}`]));
});
test('apply keeps a conflicting merge in the registry', t => {
  const f = fixture(t), r = f.marker(), a = sub(r, 'a'); f.registry([entry(a)]); f.put(`path:${a}`, [row('id', 'new')]); f.put(`path:${r}`, [row('id', 'old')]);
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr); const o = JSON.parse(result.stdout).outcomes[0]; assert.equal(o.outcome, 'conflict'); assert.deepEqual(o.conflicts, ['id']); assert.equal(o.conflicts_total, 1); assert.ok(f.entries().some(x => x.project === `path:${a}`));
});
test('plan drops ignored entries whose directory is gone', t => { const f = fixture(t), x = join(f.ignored, 'gone'); f.registry([entry(x)]); assert.deepEqual(f.inspect().drops, [entry(x)]); });
test('plan never merges repo entries', t => { const f = fixture(t), r = f.marker(), a = sub(r, 'a'); f.registry([{ project: 'repo:old/repo', cwd: a }]); const p = f.inspect(); assert.deepEqual(p.merges, []); assert.deepEqual(p.changed_repos, [{ project: 'repo:old/repo', cwd: a, resolved: `path:${r}` }]); });
test('inspect does not create missing source stores', t => { const f = fixture(t), r = f.marker(), a = sub(r, 'a'); f.registry([entry(a)]); assert.equal(f.inspect().merges[0].sources, 0); assert.equal(existsSync(f.store(`path:${a}`)), false); });
test('conflicts leave drops registered and absent stores do not create targets', t => {
  const f = fixture(t), r = f.marker(), a = sub(r, 'a'), b = sub(r, 'b'), x = sub(f.ignored, 'x');
  f.registry([entry(a), entry(b), entry(x)]); f.put(`path:${a}`, [row('id', 'new')]); f.put(`path:${r}`, [row('id', 'old')]);
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr);
  const report = JSON.parse(result.stdout); assert.deepEqual(report.outcomes.map(o => o.outcome), ['conflict', 'merged']); assert.equal(report.dropped, 0);
  assert.ok(f.entries().some(e => e.project === `path:${x}`)); assert.ok(!f.entries().some(e => e.project === `path:${b}`)); assert.equal(existsSync(f.store(`path:${b}`)), false);
});
test('an absent source store does not create or register a target', t => {
  const f = fixture(t), r = f.marker(), a = sub(r, 'a'); f.registry([entry(a)]);
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr);
  const o = JSON.parse(result.stdout).outcomes[0]; assert.equal(o.outcome, 'merged'); assert.equal(o.inserted, 0); assert.equal(existsSync(f.store(`path:${r}`)), false); assert.deepEqual(f.entries(), []);
});
test('a merge into an unregistered target at registry capacity is skipped without import', t => {
  const f = fixture(t, { fake: { FAKE_OPERATOR_HEADROOM: 0 } }), r = f.marker(), a = sub(r, 'a');
  const originals = [entry(a), ...Array.from({ length: 255 }, (_, i) => entry(join(f.root, `missing${i}`)))];
  f.registry(originals); f.put(`path:${a}`, [row('s')]);
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr);
  const outcome = JSON.parse(result.stdout).outcomes[0]; assert.equal(outcome.outcome, 'registry_full'); assert.equal(existsSync(f.store(`path:${r}`)), false);
  assert.deepEqual(new Set(f.entries().map(e => e.project)), new Set(originals.map(e => e.project)));
});
test('chunked imports sum results and preserve line order', t => {
  const f = fixture(t, { limits: { chunkLines: 3, chunkBytes: 1024 * 1024 } }), r = f.marker(), a = sub(r, 'a'), log = join(f.root, 'imports');
  f.configure({ FAKE_OPERATOR_LINE_LIMIT: 3, FAKE_OPERATOR_IMPORT_LOG: log });
  f.registry([entry(a)]); f.put(`path:${a}`, Array.from({ length: 7 }, (_, i) => row(`s${i}`))); f.put(`path:${r}`, [row('s0')]);
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr);
  const outcome = JSON.parse(result.stdout).outcomes[0]; assert.equal(outcome.inserted, 6); assert.equal(outcome.duplicates, 1);
  assert.deepEqual(readFileSync(log, 'utf8').trim().split('\n').map(Number), [3, 3, 1]);
  assert.deepEqual(f.get(`path:${r}`).map(x => x.source), Array.from({ length: 7 }, (_, i) => `s${i}`));
});
test('chunk results sum conflicts, rejections and remaining', t => {
  const f = fixture(t, { limits: { chunkLines: 3 } }), r = f.marker(), a = sub(r, 'a');
  f.configure({ FAKE_OPERATOR_HEADROOM: 1, FAKE_OPERATOR_REJECT_SOURCE: 's2' }); f.registry([entry(a)]);
  f.put(`path:${a}`, Array.from({ length: 7 }, (_, i) => row(`s${i}`))); f.put(`path:${r}`, [row('s0'), row('s1', 'different')]);
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr); const outcome = JSON.parse(result.stdout).outcomes[0];
  assert.equal(outcome.outcome, 'conflict'); assert.equal(outcome.inserted, 2); assert.equal(outcome.duplicates, 1); assert.equal(outcome.rejected, 1);
  assert.equal(outcome.remaining, 2); assert.deepEqual(outcome.conflicts, ['s1']); assert.equal(outcome.conflicts_total, 1);
});
test('chunk failure stops imports and rerun counts prior writes as duplicates', t => {
  const f = fixture(t, { limits: { chunkLines: 3, chunkBytes: 1024 * 1024 } }), r = f.marker(), a = sub(r, 'a'), log = join(f.root, 'imports');
  f.configure({ FAKE_OPERATOR_LINE_LIMIT: 3, FAKE_OPERATOR_IMPORT_LOG: log, FAKE_OPERATOR_FAIL_CHUNK: 2 });
  f.registry([entry(a)]); f.put(`path:${a}`, Array.from({ length: 7 }, (_, i) => row(`s${i}`)));
  const first = f.apply(f.inspect()); assert.equal(first.status, 2); assert.equal(JSON.parse(first.stdout).outcomes[0].outcome, 'failed');
  assert.deepEqual(readFileSync(log, 'utf8').trim().split('\n').map(Number), [3, 3]); assert.equal(f.get(`path:${r}`).length, 6); assert.ok(f.entries().some(e => e.project === `path:${a}`));
  f.configure({ FAKE_OPERATOR_LINE_LIMIT: 3 }); const second = f.apply(f.inspect(), join(f.root, 'backup2')); assert.equal(second.status, 0, second.stderr);
  const result = JSON.parse(second.stdout).outcomes[0]; assert.equal(result.duplicates, 6); assert.equal(result.inserted, 1);
});
test('chunks also respect byte limits without splitting UTF-8 lines', t => {
  const f = fixture(t, { limits: { chunkLines: 50, chunkBytes: 350 } }), r = f.marker(), a = sub(r, 'a'), log = join(f.root, 'imports');
  f.configure({ FAKE_OPERATOR_IMPORT_LOG: log }); f.registry([entry(a)]);
  const rows = Array.from({ length: 7 }, (_, i) => row(`s${i}`, '🧠'.repeat(15))); f.put(`path:${a}`, rows);
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr); assert.deepEqual(f.get(`path:${r}`), rows);
  assert.ok(readFileSync(log, 'utf8').trim().split('\n').length > 1);
});
test('a full registry skips the import and keeps every entry', t => {
  const f = fixture(t), r = f.marker(), a = sub(r, 'a');
  const missing = Array.from({ length: 255 }, (_, i) => entry(join(f.root, `missing${i}`)));
  f.registry([entry(a), ...missing]); f.put(`path:${a}`, [row('s')]);
  const result = f.apply(f.inspect()); assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).outcomes[0].outcome, 'registry_full');
  assert.equal(existsSync(f.store(`path:${r}`)), false);
  assert.deepEqual(new Set(f.entries().map(e => e.project)), new Set([`path:${a}`, ...missing.map(e => e.project)]));
});
test('apply refuses an existing backup directory', t => { const f = fixture(t), r = f.marker(); f.registry([entry(r)]); const backup = sub(f.root, 'backup'); const before = readFileSync(join(f.data, 'projects.json'), 'utf8'); const result = f.apply(f.inspect(), backup); assert.equal(result.status, 2); assert.equal(readFileSync(join(f.data, 'projects.json'), 'utf8'), before); });

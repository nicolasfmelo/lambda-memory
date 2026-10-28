import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { resolveProjectBinding } from '../runtime/project-binding.mjs';

function fixture(t, active = false) {
  const root = mkdtempSync(join(tmpdir(), 'lm-tree-migration-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin'), data = join(root, 'data'); mkdirSync(bin); mkdirSync(data);
  const dirs = ['one', 'two', 'three'].map(name => { const path = join(root, name); mkdirSync(path); return path; });
  const projects = dirs.map(cwd => ({ ...resolveProjectBinding(cwd), last_seen_ms: 1 }));
  writeFileSync(join(data, 'projects.json'), JSON.stringify({ projects }), { mode: 0o600 });
  writeFileSync(join(bin, 'systemctl'), `#!/bin/sh\necho ${active ? 'active' : 'inactive'}\n`, { mode: 0o755 });
  const native = join(bin, 'native');
  writeFileSync(native, `#!/usr/bin/env node\nconst p=process.env.LAMBDA_MEMORY_PROJECT; if(process.argv[2]==='--inspect-tree') console.log(JSON.stringify({project:p,mode:'flat',flat_memories:1,sources:['s'],batches:1,accept_charges:false})); else {let x='';process.stdin.on('data',c=>x+=c);process.stdin.on('end',()=>{if(process.cwd().endsWith('/two')) process.exit(2);console.log(JSON.stringify({project:p,outcome:'succeeded'}));});}\n`, { mode: 0o755 });
  const config = join(root, 'config.json');
  writeFileSync(config, JSON.stringify({ nativeBin: native, bridgeBin: native, dataDir: data, socketPath: join(root, 's'), owner: 'owner', actor: 'actor', token: 'x'.repeat(32) }), { mode: 0o600 });
  const env = { ...process.env, PATH: `${bin}:${process.env.PATH}`, LAMBDA_MEMORY_CONFIG: config };
  const run = (action, input, args = []) => spawnSync(process.execPath, [resolve('runtime/migrate-all-trees.mjs'), action, ...args], { env, input, encoding: 'utf8' });
  return { run, projects, dirs };
}
test('plan_aggregates_projects', t => {
  const { run, projects, dirs } = fixture(t); rmSync(dirs[2], { recursive: true });
  const result = run('plan'); assert.equal(result.status, 0, result.stderr);
  const plan = JSON.parse(result.stdout); assert.equal(plan.projects.length, 2); assert.equal(plan.total_batches, 2);
  assert.equal(plan.skipped[0].project, projects[2].project); assert.equal(plan.skipped[0].reason, 'missing_cwd');
});
test('apply_stops_at_first_failure', t => {
  const { run } = fixture(t); const planned = run('plan'); assert.equal(planned.status, 0, planned.stderr);
  const plan = JSON.parse(planned.stdout); plan.accept_charges = true;
  const result = run('apply', JSON.stringify(plan)); assert.equal(result.status, 2, result.stderr);
  const report = JSON.parse(result.stdout); assert.deepEqual(report.outcomes.map(x => x.outcome), ['succeeded', 'succeeded', 'failed']);
});
test('apply_refuses_when_service_active', t => {
  const { run } = fixture(t, true); const result = run('apply', '{}');
  assert.equal(result.status, 2); assert.match(result.stderr, /service to be stopped/);
});
test('plan_skips_entries_whose_binding_changed', t => {
  const { run, projects } = fixture(t);
  const registry = join(resolve(projects[0].cwd, '..'), 'data', 'projects.json');
  const stale = { project: 'path:/old/identity', cwd: projects[0].cwd, last_seen_ms: 1 };
  writeFileSync(registry, JSON.stringify({ projects: [stale, ...projects.slice(1)] }), { mode: 0o600 });
  const result = run('plan'); assert.equal(result.status, 0, result.stderr);
  const plan = JSON.parse(result.stdout);
  assert.equal(plan.projects.length, 2);
  assert.deepEqual(plan.skipped.map(s => [s.project, s.reason]), [['path:/old/identity', 'binding_changed']]);
});
test('plan_only_and_exclude_select_projects', t => {
  const { run, projects } = fixture(t);
  const result = run('plan', undefined, ['--only', projects[0].project, '--only', projects[1].project, '--exclude', projects[1].project]);
  assert.equal(result.status, 0, result.stderr);
  const plan = JSON.parse(result.stdout);
  assert.deepEqual(plan.projects.map(p => p.project), [projects[0].project]);
  assert.deepEqual(plan.skipped.map(s => s.reason).sort(), ['excluded', 'not_selected']);
});
test('summary_is_readable_and_reports_consent', t => {
  const { run } = fixture(t);
  const plan = JSON.parse(run('plan').stdout);
  const result = run('summary', JSON.stringify(plan));
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Projects to curate \(paid\): 3, curation batches: 3/);
  assert.match(result.stdout, /Consent: not given/);
});
test('unknown_flag_is_rejected', t => {
  const { run } = fixture(t);
  const result = run('plan', undefined, ['--all']);
  assert.equal(result.status, 2); assert.match(result.stderr, /usage:/);
});

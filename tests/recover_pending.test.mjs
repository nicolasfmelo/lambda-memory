import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { resolveProjectBinding } from '../runtime/project-binding.mjs';

// Fake native records its argv and stdin; fake systemctl reports the service state.
function fixture(t, active = false) {
  const root = mkdtempSync(join(tmpdir(), 'lm-recover-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin'), project = join(root, 'project'), log = join(root, 'native.log');
  mkdirSync(bin); mkdirSync(project);
  writeFileSync(join(bin, 'systemctl'), `#!/bin/sh\necho ${active ? 'active' : 'inactive'}\n`, { mode: 0o755 });
  const native = join(bin, 'native');
  writeFileSync(native, `#!/usr/bin/env node\nlet x='';process.stdin.on('data',c=>x+=c);process.stdin.on('end',()=>{require('fs').writeFileSync(${JSON.stringify(log)},JSON.stringify({argv:process.argv.slice(2),stdin:x}));console.log('{}');});\n`, { mode: 0o755 });
  const config = join(root, 'runtime.json');
  writeFileSync(config, JSON.stringify({ nativeBin: native, bridgeBin: native, dataDir: join(root, 'data'), socketPath: join(root, 's'), owner: 'owner', actor: 'actor', token: 'x'.repeat(32), ignoreRoots: [] }), { mode: 0o600 });
  const run = (action, input = '') => spawnSync(process.execPath, [resolve('runtime/recover-pending.mjs'), action],
    { env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, LAMBDA_MEMORY_CONFIG: config, LAMBDA_MEMORY_CWD: project }, input, encoding: 'utf8' });
  const logged = () => JSON.parse(readFileSync(log, 'utf8'));
  return { run, logged, project: resolveProjectBinding(project).project };
}

test('timeline backfill maps to the dry run and to --apply', t => {
  const f = fixture(t);
  assert.equal(f.run('inspect-timeline-backfill').status, 0);
  assert.deepEqual(f.logged().argv, ['--backfill-timeline']);
  assert.equal(f.run('timeline-backfill').status, 0);
  assert.deepEqual(f.logged().argv, ['--backfill-timeline', '--apply']);
});

test('paid backfills require the exact project and accept_charges', t => {
  const f = fixture(t);
  for (const action of ['lesson-backfill', 'reflection-backfill']) {
    assert.equal(f.run(action, JSON.stringify({ project: f.project, accept_charges: false })).status, 2);
    assert.equal(f.run(action, JSON.stringify({ project: 'other', accept_charges: true })).status, 2);
    const ok = f.run(action, JSON.stringify({ project: f.project, accept_charges: true }));
    assert.equal(ok.status, 0, ok.stderr);
    assert.deepEqual(f.logged().argv, [action === 'lesson-backfill' ? '--lesson-backfill' : '--reflection-backfill']);
  }
  assert.equal(f.run('inspect-lesson-backfill').status, 0);
  assert.deepEqual(f.logged().argv, ['--inspect-lesson-backfill']);
});

test('backfills refuse while the service is active', t => {
  const f = fixture(t, true);
  const result = f.run('inspect-reflection-backfill');
  assert.equal(result.status, 2);
  assert.match(result.stderr, /service to be stopped/);
});

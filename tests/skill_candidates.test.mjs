import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { loadConfig } from '../runtime/project-binding.mjs';
import { installSkillFile } from '../runtime/skill-install.mjs';

const cli = resolve('runtime/skill-candidates.mjs'), service = resolve('runtime/local-service.mjs');
const fake = resolve('tests/support/fake-native.cjs');
const hash = text => createHash('sha256').update(text).digest('hex');
const draft = '# Skill\nFollow steps.\n';
async function fixture(t, row = {}, rejectDecision = '') {
  const root = mkdtempSync(join(tmpdir(), 'lm-skill-')), home = join(root, 'home'), project = join(root, 'project');
  mkdirSync(home); mkdirSync(project);
  const state = join(root, 'state.json'), log = join(root, 'log');
  writeFileSync(state, JSON.stringify([{ candidate_id: 'candidate', scope: 'project', revision_id: 'rev1', slug: 'example-skill', status: 'drafted', draft, install_paths: [], install_hashes: [], ...row }]));
  const wrapper = join(root, 'native.cjs');
  writeFileSync(wrapper, `#!/usr/bin/env node\nObject.assign(process.env, ${JSON.stringify({ FAKE_NATIVE_SKILLS: state, FAKE_NATIVE_LOG: log, FAKE_NATIVE_SKILL_REJECT_DECISION: rejectDecision })});\nrequire(${JSON.stringify(fake)});\n`, { mode: 0o755 });
  const config = join(root, 'config.json'), data = join(root, 'data'), socket = join(root, 'run', 'socket');
  const settings = { nativeBin: wrapper, bridgeBin: wrapper, dataDir: data, socketPath: socket, owner: 'owner', actor: 'actor', token: '01234567890123456789012345678901', ignoreRoots: [], skillTargets: { project: ['.claude/skills'], user: [join(home, '.claude/skills'), join(home, '.omp/agent/managed-skills')] } };
  writeFileSync(config, JSON.stringify(settings), { mode: 0o600 });
  const env = { ...process.env, HOME: home, LAMBDA_MEMORY_CONFIG: config, NOTIFY_SOCKET: '' };
  const child = spawn(process.execPath, [service], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(async () => { child.kill('SIGTERM'); await new Promise(done => child.exitCode !== null ? done() : child.once('exit', done)); rmSync(root, { recursive: true, force: true }); });
  await new Promise((done, fail) => { child.stdout.on('data', text => { if (String(text).includes('ready')) done(); }); child.once('exit', code => fail(new Error(`service exited ${code}`))); });
  const raw = (...args) => spawnSync(process.execPath, [cli, ...args], { cwd: project, env, encoding: 'utf8' });
  const run = (...args) => raw(...args, ...(args[0] === 'approve' && args.includes('--confirm') ? ['--draft', hash(draft).slice(0, 16)] : []));
  const calls = () => existsSync(log) ? readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse) : [];
  const save = rows => writeFileSync(state, JSON.stringify(rows));
  const get = () => JSON.parse(readFileSync(state, 'utf8'))[0];
  return { root, home, project, data, config, settings, env, run, raw, calls, save, get };
}

test('list prints candidates for the project and with --user for the user store', async t => {
  const f = await fixture(t);
  assert.equal(JSON.parse(f.run('list').stdout).scope, 'project');
  const result = f.run('list', '--user'); assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).project, 'user:owner');
  assert.equal(f.calls().filter(c => c.name === 'memory_skill_candidates').at(-1).project, 'user:owner');
});
test('approve requires --confirm and the shown revision', async t => {
  const f = await fixture(t); assert.match(f.run('show', 'candidate').stdout, new RegExp(`revision: rev1\\ndraft: ${hash(draft).slice(0, 16)}\\n$`));
  assert.equal(f.run('approve', 'candidate', '--revision', 'rev1').status, 2);
  assert.equal(f.run('approve', 'candidate', '--revision', 'wrong', '--confirm').status, 2);
  assert.equal(f.calls().some(c => c.name === 'memory_skill_decide'), false);
});
test('approval is bound to the exact shown draft', async t => {
  const f = await fixture(t);
  const shown = f.run('show', 'candidate').stdout;
  assert.match(shown, new RegExp(`draft: ${hash(draft).slice(0, 16)}\\n`));
  const missing = f.raw('approve', 'candidate', '--revision', 'rev1', '--confirm');
  assert.equal(missing.status, 2);
  assert.match(missing.stderr, /approve requires --revision <rev>, --draft <hash> and --confirm/);
  const invalid = f.raw('approve', 'candidate', '--revision', 'rev1', '--draft', 'ABC', '--confirm');
  assert.equal(invalid.status, 2);
  const wrong = f.raw('approve', 'candidate', '--revision', 'rev1', '--draft', '0'.repeat(16), '--confirm');
  assert.equal(wrong.status, 2); assert.match(wrong.stderr, /draft changed; run show again/);
  assert.equal(existsSync(join(f.project, '.claude/skills/example-skill/SKILL.md')), false);
  assert.equal(f.calls().some(c => c.name === 'memory_skill_decide'), false);
  f.save([{ ...f.get(), draft: 'different draft' }]);
  const changed = f.raw('approve', 'candidate', '--revision', 'rev1', '--draft', hash(draft).slice(0, 16), '--confirm');
  assert.equal(changed.status, 2); assert.match(changed.stderr, /draft changed; run show again/);
  assert.equal(f.calls().some(c => c.name === 'memory_skill_decide'), false);
});
test('approve writes SKILL.md to every target and records installed paths', async t => {
  const f = await fixture(t);
  assert.equal(f.run('approve', 'candidate', '--revision', 'rev1', '--confirm').status, 0);
  const path = join(f.project, '.claude/skills/example-skill/SKILL.md'); assert.equal(readFileSync(path, 'utf8'), draft);
  assert.deepEqual(f.get().install_paths, [path]); assert.deepEqual(f.get().install_hashes, [hash(draft)]);
  assert.deepEqual(f.calls().filter(c => c.name === 'memory_skill_decide').map(c => c.arguments.revision_id), ['rev1', 'rev1']);
  f.save([{ ...f.get(), status: 'drafted', install_paths: [], install_hashes: [] }]);
  const user = f.run('approve', 'candidate', '--user', '--revision', 'rev1', '--confirm'); assert.equal(user.status, 0, user.stderr);
  for (const dir of ['.claude/skills', '.omp/agent/managed-skills']) assert.equal(readFileSync(join(f.home, dir, 'example-skill/SKILL.md'), 'utf8'), draft);
});
test('approve refuses when an existing different file is present and writes nothing', async t => {
  const f = await fixture(t); const second = join(f.home, '.omp/agent/managed-skills/example-skill'); mkdirSync(second, { recursive: true }); writeFileSync(join(second, 'SKILL.md'), 'personal');
  const result = f.run('approve', 'candidate', '--user', '--revision', 'rev1', '--confirm'); assert.equal(result.status, 2); assert.match(result.stderr, /existing skill differs/);
  assert.equal(existsSync(join(f.home, '.claude/skills/example-skill/SKILL.md')), false); assert.equal(readFileSync(join(second, 'SKILL.md'), 'utf8'), 'personal');
});
test('approve refuses symlinked skill directories without writing outside the target', async t => {
  const f = await fixture(t); const target = join(f.project, '.claude/skills'); mkdirSync(target, { recursive: true });
  const outside = join(f.root, 'outside'); mkdirSync(outside); symlinkSync(outside, join(target, 'example-skill'));
  const result = f.run('approve', 'candidate', '--revision', 'rev1', '--confirm');
  assert.equal(result.status, 2); assert.match(result.stderr, /existing skill differs: .*symlinked path/);
  assert.equal(existsSync(join(outside, 'SKILL.md')), false);
});
test('approve keeps an identical existing file', async t => {
  const f = await fixture(t); const dir = join(f.project, '.claude/skills/example-skill'); mkdirSync(dir, { recursive: true }); const path = join(dir, 'SKILL.md'); writeFileSync(path, draft);
  assert.equal(f.run('approve', 'candidate', '--revision', 'rev1', '--confirm').status, 0); assert.equal(readFileSync(path, 'utf8'), draft);
});
test("approve replaces only Lambda's own unchanged earlier file", async t => {
  const f = await fixture(t); const dir = join(f.project, '.claude/skills/example-skill'); mkdirSync(dir, { recursive: true }); const path = join(dir, 'SKILL.md'); writeFileSync(path, 'old');
  f.save([{ ...f.get(), install_paths: [path], install_hashes: [hash('old')] }]);
  const result = f.run('approve', 'candidate', '--revision', 'rev1', '--confirm');
  assert.equal(result.status, 0, result.stderr); assert.equal(readFileSync(path, 'utf8'), draft);
  const backup = result.stdout.trim().replace(/^backup: /, ''); assert.equal(readFileSync(backup, 'utf8'), 'old');
  writeFileSync(path, 'edited'); f.save([{ ...f.get(), status: 'drafted' }]);
  assert.equal(f.run('approve', 'candidate', '--revision', 'rev1', '--confirm').status, 2); assert.equal(readFileSync(path, 'utf8'), 'edited');
});
test('replacement keeps a backup and the skill exists at every step', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'lm-replace-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const path = join(dir, 'SKILL.md'); writeFileSync(path, 'old'); let backup; const steps = [];
  const check = stage => { assert.ok(existsSync(path)); steps.push(stage); };
  await installSkillFile(path, Buffer.from(draft), hash('old'), () => check('written'), {
    afterTemp: async () => check('temp'), afterBackup: async name => { backup = name; check('backup'); },
    beforeRename: async () => check('before rename'),
  });
  assert.deepEqual(steps, ['temp', 'backup', 'before rename', 'written']);
  assert.equal(readFileSync(path, 'utf8'), draft); assert.equal(readFileSync(backup, 'utf8'), 'old');
  assert.match(backup, /\.SKILL\.md\.lambda-backup-/);
});
test('replacement refuses an in-place edit immediately before rename', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'lm-replace-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const path = join(dir, 'SKILL.md'); writeFileSync(path, 'old'); let backup;
  await assert.rejects(installSkillFile(path, draft, hash('old'), () => {}, {
    beforeRename: async name => { backup = name; writeFileSync(path, 'user edit'); },
  }), /existing skill changed during approve/);
  assert.equal(readFileSync(path, 'utf8'), 'user edit');
  assert.ok(existsSync(backup));
});
test('replacement refuses a hash mismatch found after the backup', async t => {
  const dir = mkdtempSync(join(tmpdir(), 'lm-replace-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const path = join(dir, 'SKILL.md'); writeFileSync(path, 'old');
  await assert.rejects(installSkillFile(path, Buffer.from(draft), hash('old'), () => {}, {
    afterBackup: async () => writeFileSync(path, 'user edit'),
  }), /existing skill differs/);
  assert.equal(readFileSync(path, 'utf8'), 'user edit');
  assert.equal(readdirSync(dir).some(name => name.endsWith('.tmp')), false);
});
test('approve refuses an error from either revision-bound decision', async t => {
  for (const decision of ['approved', 'installed']) {
    const f = await fixture(t, {}, decision);
    const result = f.run('approve', 'candidate', '--revision', 'rev1', '--confirm');
    assert.equal(result.status, 2); assert.match(result.stderr, /skill revision mismatch/);
    assert.deepEqual(f.calls().filter(c => c.name === 'memory_skill_decide').map(c => c.arguments.revision_id),
      decision === 'approved' ? ['rev1'] : ['rev1', 'rev1']);
    assert.notEqual(f.get().status, 'installed');
  }
});
test('approve recovers an approved row', async t => {
  const f = await fixture(t, { status: 'approved' }); assert.equal(f.run('approve', 'candidate', '--revision', 'rev1', '--confirm').status, 0);
  assert.deepEqual(f.calls().filter(c => c.name === 'memory_skill_decide').map(c => c.arguments.decision), ['installed']);
});
test('--user binds to the user store without touching the registry', async t => {
  const f = await fixture(t); assert.equal(f.run('list', '--user').status, 0); assert.equal(existsSync(join(f.data, 'projects.json')), false);
});
test('export writes drafts under the candidates directory', async t => {
  const f = await fixture(t); const result = f.run('export'); assert.equal(result.status, 0, result.stderr);
  const path = join(f.home, '.local/share/lambda-memory/skill-candidates/project/example-skill/SKILL.md'); assert.equal(readFileSync(path, 'utf8'), draft); assert.match(result.stdout, /SKILL.md/);
});
test('reject records the decision', async t => {
  const f = await fixture(t); assert.equal(f.run('reject', 'candidate').status, 0); assert.equal(f.get().status, 'rejected');
  assert.equal(f.calls().find(c => c.name === 'memory_skill_decide').arguments.revision_id, 'rev1');
});
test('list marks stale installed candidates outdated', async t => {
  const f = await fixture(t, { status: 'stale', install_paths: ['/installed'] });
  assert.equal(JSON.parse(f.run('list').stdout).candidates[0].outdated, true);
});
test('configured project targets are resolved against the project binding', async t => {
  const f = await fixture(t);
  writeFileSync(f.config, JSON.stringify({ ...f.settings, skillTargets: { project: ['skills/one', 'skills/two'], user: f.settings.skillTargets.user } }));
  const result = f.run('approve', 'candidate', '--revision', 'rev1', '--confirm'); assert.equal(result.status, 0, result.stderr);
  for (const dir of ['skills/one', 'skills/two']) assert.equal(readFileSync(join(f.project, dir, 'example-skill/SKILL.md'), 'utf8'), draft);
});
test('export refuses a symlinked candidate directory', async t => {
  const f = await fixture(t); const base = join(f.home, '.local/share/lambda-memory/skill-candidates/project');
  mkdirSync(base, { recursive: true }); symlinkSync(f.project, join(base, 'example-skill'));
  assert.equal(f.run('export').status, 2); assert.equal(existsSync(join(f.project, 'SKILL.md')), false);
});
test('ignored project directories are rejected', async t => {
  const f = await fixture(t); writeFileSync(f.config, JSON.stringify({ ...f.settings, ignoreRoots: [f.project] }));
  const result = f.run('list'); assert.equal(result.status, 2); assert.match(result.stderr, /ignored directory/);
});
test('skillTargets config is validated', async t => {
  const f = await fixture(t);
  for (const skillTargets of [{ user: ['relative'] }, { project: ['a/../b'] }, { project: Array(5).fill('a') }, { user: ['/bad\u007f'] }]) {
    writeFileSync(f.config, JSON.stringify({ ...f.settings, skillTargets })); assert.throws(() => loadConfig(f.env), /invalid configuration/);
  }
});

test('approve aborts when SKILL.md is replaced by a new file before the rename', async (t) => {
  const { installSkillFile } = await import('../runtime/skill-install.mjs');
  const { mkdtempSync, writeFileSync, readFileSync, renameSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { createHash } = await import('node:crypto');
  const dir = mkdtempSync(join(tmpdir(), 'lm-skill-race-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const path = join(dir, 'SKILL.md');
  writeFileSync(path, 'old draft');
  const recorded = createHash('sha256').update('old draft').digest('hex');
  await assert.rejects(installSkillFile(path, 'new draft', recorded, () => {}, {
    beforeRename: async () => { writeFileSync(join(dir, 'edit.tmp'), 'user edit'); renameSync(join(dir, 'edit.tmp'), path); },
  }), /changed during approve/);
  assert.equal(readFileSync(path, 'utf8'), 'user edit');
});

import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, mkdirSync, renameSync, rmSync, symlinkSync, writeFileSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { projectSkillRoot, planSkillInstall, applySkillInstall } from '../runtime/skill-install.mjs';

test('linked worktrees use the main worktree', async t => {
  const root = mkdtempSync(join(tmpdir(), 'lm-skill-root-')); t.after(() => rmSync(root, { recursive: true, force: true }));
  const main = join(root, 'main'), linked = join(root, 'linked'); mkdirSync(main);
  execFileSync('git', ['init', '-q', main]);
  execFileSync('git', ['-C', main, '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '--allow-empty', '-qm', 'initial']);
  execFileSync('git', ['-C', main, 'worktree', 'add', '-q', '-b', 'linked', linked]);
  assert.equal(await projectSkillRoot(linked), main);
  assert.equal(await projectSkillRoot(root), root);
});

test('symlink target roots are accepted but symlinked skills and files are blocked', async t => {
  const root = mkdtempSync(join(tmpdir(), 'lm-skill-plan-')); t.after(() => rmSync(root, { recursive: true, force: true }));
  const target = join(root, 'target'), alias = join(root, 'alias'), outside = join(root, 'outside'); mkdirSync(target); mkdirSync(outside); symlinkSync(target, alias);
  const row = { slug: 'example', draft: 'draft', install_paths: [], install_hashes: [] };
  const plan = () => planSkillInstall({ row, scope: 'user', targets: [alias], projectRoot: root });
  const path = join(alias, 'example/SKILL.md');
  assert.deepEqual(await plan(), [{ path, action: 'create' }]);
  await applySkillInstall(await plan(), row.draft); assert.equal(readFileSync(path, 'utf8'), 'draft');
  rmSync(join(target, 'example'), { recursive: true }); symlinkSync(outside, join(target, 'example'));
  assert.deepEqual(await plan(), [{ path, action: 'blocked', reason: 'symlinked path' }]);
  rmSync(join(target, 'example')); mkdirSync(join(target, 'example')); writeFileSync(join(outside, 'SKILL.md'), 'outside'); symlinkSync(join(outside, 'SKILL.md'), path);
  assert.deepEqual(await plan(), [{ path, action: 'blocked', reason: 'symlinked path' }]);
});

test('symlinked configured root preserves previous install paths on replacement', async t => {
  const root = mkdtempSync(join(tmpdir(), 'lm-skill-alias-')); t.after(() => rmSync(root, { recursive: true, force: true }));
  const target = join(root, 'target'), alias = join(root, 'alias'); mkdirSync(join(target, 'example'), { recursive: true }); symlinkSync(target, alias);
  const path = join(alias, 'example/SKILL.md'); writeFileSync(path, 'old');
  const previous = createHash('sha256').update('old').digest('hex');
  const row = { slug: 'example', draft: 'new', install_paths: [path], install_hashes: [previous] };
  const plan = await planSkillInstall({ row, scope: 'user', targets: [alias], projectRoot: root });
  assert.deepEqual(plan, [{ path, action: 'replace', replace: previous }]);
  await applySkillInstall(plan, row.draft); assert.equal(readFileSync(path, 'utf8'), 'new');
});

test('replacement aborts with changed when the root is swapped before rename', async t => {
  const root = mkdtempSync(join(tmpdir(), 'lm-skill-swap-replace-')); t.after(() => rmSync(root, { recursive: true, force: true }));
  const target = join(root, 'target'), saved = join(root, 'saved'), outside = join(root, 'outside');
  mkdirSync(join(target, 'example'), { recursive: true }); mkdirSync(outside);
  const path = join(target, 'example/SKILL.md'); writeFileSync(path, 'old');
  const previous = createHash('sha256').update('old').digest('hex');
  const row = { slug: 'example', draft: 'new', install_paths: [path], install_hashes: [previous] };
  const plan = await planSkillInstall({ row, scope: 'user', targets: [target], projectRoot: root });
  await assert.rejects(applySkillInstall(plan, row.draft, { hooks: { beforeRename: () => { renameSync(target, saved); symlinkSync(outside, target); } } }), error => error.code === 'SKILL_CHANGED');
  assert.equal(existsSync(join(outside, 'example/SKILL.md')), false);
  assert.equal(readFileSync(join(saved, 'example/SKILL.md'), 'utf8'), 'old');
});

test('target root swapped after plan or after mkdir never writes outside', async t => {
  for (const stage of ['afterPlan', 'afterMkdir', 'afterTemp']) {
    const root = mkdtempSync(join(tmpdir(), 'lm-skill-swap-')); t.after(() => rmSync(root, { recursive: true, force: true }));
    const target = join(root, 'target'), outside = join(root, 'outside'); mkdirSync(outside);
    const row = { slug: 'example', draft: 'draft', install_paths: [], install_hashes: [] };
    const plan = await planSkillInstall({ row, scope: 'user', targets: [target], projectRoot: root });
    const swap = () => { rmSync(target, { recursive: true, force: true }); symlinkSync(outside, target); };
    if (stage === 'afterPlan') swap();
    await assert.rejects(applySkillInstall(plan, row.draft, { hooks: stage === 'afterPlan' ? {} : { [stage]: swap } }), error => error.code === 'SKILL_CHANGED');
    assert.equal(existsSync(join(outside, 'example/SKILL.md')), false, stage);
  }
});

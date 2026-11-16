import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { defaultIgnoreRoots, isIgnored, loadConfig, resolveCallBindingAsync, resolveDirectoryIdentityAsync } from '../runtime/project-binding.mjs';

function fixture(t) {
  const temp = mkdtempSync(join(tmpdir(), 'lm-binding-'));
  t.after(() => rmSync(temp, { recursive: true, force: true }));
  return realpathSync(temp);
}
function directory(path) { mkdirSync(path, { recursive: true }); return path; }
function binding(path) { return { cwd: path, project: `path:${path}` }; }
function repo(path) {
  directory(path);
  execFileSync('git', ['init', '-q', path]);
  execFileSync('git', ['-C', path, 'remote', 'add', 'origin', 'https://example.com/acme/app.git']);
  return { cwd: path, project: 'repo:example.com/acme/app' };
}
const call = (cwd, sessionRoot, ignoreRoots = []) => resolveCallBindingAsync(cwd, { sessionRoot, ignoreRoots });
const consolidate = (dir, ignoreRoots = []) => resolveDirectoryIdentityAsync(dir, { ignoreRoots });

test('subfolder without git binds to the session root', async t => {
  const temp = fixture(t);
  for (const name of ['plain', 'Enty lang']) {
    const root = directory(join(temp, name));
    assert.deepEqual(await call(directory(join(root, 'a/b')), root), binding(root));
  }
});

test('nearest marker below the session root wins', async t => {
  const temp = fixture(t);
  for (const marker of ['CLAUDE.md', 'AGENTS.md', '.lambda-memory']) {
    const root = directory(join(temp, marker.replace('.', 'dot')));
    const parent = directory(join(root, 'a'));
    const cwd = directory(join(parent, 'b'));
    writeFileSync(join(root, 'CLAUDE.md'), '');
    if (marker === '.lambda-memory') directory(join(parent, marker));
    else writeFileSync(join(parent, marker), '');
    assert.deepEqual(await call(cwd, root), binding(parent));
  }
});

test('a git repository inside the session root keeps its identity', async t => {
  const root = fixture(t);
  const expected = repo(join(root, 'app'));
  const cwd = directory(join(expected.cwd, 'src'));
  assert.deepEqual(await call(cwd, root), expected);
  execFileSync('git', ['-C', expected.cwd, 'remote', 'add', 'upstream', 'https://user:password@example.com/upstream/app.git']);
  assert.deepEqual(await call(cwd, root), { cwd: expected.cwd, project: 'repo:example.com/upstream/app' });
});

test('ignored roots bind to nothing', async t => {
  const root = fixture(t);
  const ignored = directory(join(root, 'ignored'));
  assert.equal(await call(directory(join(ignored, 'sub')), root, [ignored]), null);
  assert.equal(isIgnored(ignored, [ignored]), true);
  assert.equal(isIgnored(`${ignored}x`, [ignored]), false);
});

test('cwd outside the session root resolves on its own', async t => {
  const temp = fixture(t);
  const root = directory(join(temp, 'R'));
  const other = directory(join(temp, 'S'));
  const cwd = directory(join(other, 'x'));
  assert.deepEqual(await call(cwd, root), binding(cwd));
  writeFileSync(join(other, 'AGENTS.md'), '');
  assert.deepEqual(await call(cwd, root), binding(other));
});

test('missing session root falls back to the call directory', async t => {
  const cwd = directory(join(fixture(t), 'a/b'));
  assert.deepEqual(await call(cwd, undefined), binding(cwd));
  assert.deepEqual(await call(cwd, join(cwd, 'missing')), binding(cwd));
  assert.deepEqual(await call(cwd, 'relative'), binding(cwd));
});

test('symlinked session root contains real-path calls', async t => {
  const temp = fixture(t);
  const root = directory(join(temp, 'R'));
  const link = join(temp, 'link');
  symlinkSync(root, link);
  assert.deepEqual(await call(directory(join(root, 'a')), link), binding(root));
});

test('consolidation resolver', async t => {
  const temp = fixture(t);
  const root = directory(join(temp, 'R'));
  const child = directory(join(root, 'a/b'));
  writeFileSync(join(root, 'CLAUDE.md'), '');
  assert.deepEqual(await consolidate(child), { action: 'merge', binding: binding(root) });
  const expected = repo(join(temp, 'repo'));
  assert.deepEqual(await consolidate(expected.cwd), { action: 'keep', binding: expected });
  assert.deepEqual(await consolidate(directory(join(expected.cwd, 'sub'))), { action: 'merge', binding: expected });
  assert.deepEqual(await consolidate(child, [root]), { action: 'drop' });
  assert.deepEqual(await consolidate(join(temp, 'missing')), { action: 'missing' });
  await t.test('no marker or Git ancestor keeps the directory', async t => {
    for (let current = temp; ; current = dirname(current)) {
      if (['CLAUDE.md', 'AGENTS.md', '.lambda-memory', '.git'].some(name => existsSync(join(current, name)))) {
        t.skip('marker or Git ancestor exists above the test directory'); return;
      }
      if (dirname(current) === current) break;
    }
    const plain = directory(join(temp, 'plain'));
    assert.deepEqual(await consolidate(plain), { action: 'keep', binding: binding(plain) });
  });
});

test('loadConfig ignoreRoots', t => {
  const home = fixture(t);
  const config = join(home, 'runtime.json');
  const base = { nativeBin: process.execPath, bridgeBin: process.execPath, dataDir: home, socketPath: join(home, 's.sock'), owner: 'owner', actor: 'actor', token: 'a'.repeat(32) };
  const load = extra => {
    writeFileSync(config, JSON.stringify({ ...base, ...extra }), { mode: 0o600 });
    return loadConfig({ HOME: home, LAMBDA_MEMORY_CONFIG: config });
  };
  assert.ok(load({}).ignoreRoots.includes(join(home, '.cache')));
  assert.deepEqual(load({ ignoreRoots: [home] }).ignoreRoots, [home]);
  for (const invalid of [['relative'], 'x', Array(33).fill(home), [`${home}/bad\u007f`], [42]]) {
    assert.throws(() => load({ ignoreRoots: invalid }), /invalid configuration/);
  }
  const link = join(home, 'link');
  symlinkSync(home, link);
  const roots = defaultIgnoreRoots({ XDG_CACHE_HOME: link, XDG_RUNTIME_DIR: home }, home);
  assert.equal(roots.filter(root => root === home).length, 1);
  assert.ok(roots.includes(realpathSync(tmpdir())));
  assert.ok(roots.includes(join(home, '.openmausbot/task-workspaces')));
});

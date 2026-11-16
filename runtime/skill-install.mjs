import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { link, lstat, mkdir, readFile, realpath, rename, unlink, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { promisify } from 'node:util';

const git = promisify(execFile);
const hash = content => createHash('sha256').update(content).digest('hex');
const planRoots = new WeakMap();
export const skillDraftHash = draft => hash(draft).slice(0, 16);
export function skillSlug(row) {
  if (typeof row.slug !== 'string' || !/^[a-z0-9]+(-[a-z0-9]+)*$/.test(row.slug)) throw new Error('invalid skill slug');
  return row.slug;
}
export async function projectSkillRoot(cwd) {
  try {
    const { stdout } = await git('git', ['-C', cwd, 'rev-parse', '--path-format=absolute', '--git-common-dir'], { timeout: 1500, maxBuffer: 8192 });
    const common = stdout.trim();
    return basename(common) === '.git' ? dirname(common) : cwd;
  } catch { return cwd; }
}
async function existing(path) {
  try {
    const stat = await lstat(path);
    if (!stat.isFile()) throw new Error(`existing skill differs: ${path}`);
    return await readFile(path);
  } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}
async function targetRoot(path) {
  try { return await realpath(path); }
  catch (error) {
    if (error.code !== 'ENOENT') throw error;
    const parent = dirname(path);
    if (parent === path) throw error;
    return join(await targetRoot(parent), basename(path));
  }
}
async function symlinked(root, path) {
  const dir = dirname(path);
  try {
    const stat = await lstat(dir);
    if (!stat.isDirectory() || stat.isSymbolicLink()) return true;
    const rel = relative(root, await realpath(dir));
    if (!rel || rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) return true;
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  try { if ((await lstat(path)).isSymbolicLink()) return true; }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  return false;
}
export async function planSkillInstall({ row, scope, targets, projectRoot }) {
  const name = skillSlug(row);
  if (typeof row.draft !== 'string' || !['project', 'user'].includes(scope) || !Array.isArray(targets) || !targets.length) throw new Error('invalid skill targets');
  const configuredRoots = targets.map(target => scope === 'project' ? resolve(projectRoot, target) : target);
  const roots = await Promise.all(configuredRoots.map(targetRoot));
  const paths = configuredRoots.map(root => join(root, name, 'SKILL.md'));
  if (new Set(paths).size !== paths.length) throw new Error('invalid skill targets');
  const content = Buffer.from(row.draft), plans = [];
  for (let i = 0; i < paths.length; i++) {
    const path = paths[i];
    if (await symlinked(roots[i], path)) { plans.push({ path, action: 'blocked', reason: 'symlinked path' }); continue; }
    let current;
    try { current = await existing(path); }
    catch (error) { if (!String(error.message).startsWith('existing skill differs:')) throw error; plans.push({ path, action: 'blocked', reason: 'foreign file' }); continue; }
    if (!current) plans.push({ path, action: 'create' });
    else if (current.equals(content)) plans.push({ path, action: 'identical' });
    else {
      const index = row.install_paths?.indexOf(path) ?? -1, previous = index < 0 ? null : row.install_hashes?.[index];
      if (!previous) plans.push({ path, action: 'blocked', reason: 'foreign file' });
      else if (hash(current) !== previous) plans.push({ path, action: 'blocked', reason: 'edited since install' });
      else plans.push({ path, action: 'replace', replace: previous });
    }
  }
  plans.forEach((item, i) => planRoots.set(item, { configuredRoot: configuredRoots[i], resolvedRoot: roots[i] }));
  return plans;
}
// Hooks are available only to direct module callers, not through the CLI or service.
export async function installSkillFile(path, content, replace, onWritten = () => {}, hooks = {}, onBackup = () => {}) {
  const dir = dirname(path);
  await mkdir(dir, { recursive: true, mode: 0o755 });
  await hooks.afterMkdir?.();
  const temp = join(dir, `.lambda-skill-${randomUUID()}.tmp`);
  const backup = join(dir, `.SKILL.md.lambda-backup-${process.pid}-${Date.now()}${process.hrtime.bigint()}`);
  let created = false, backedUp = false;
  try {
    await writeFile(temp, content, { flag: 'wx', mode: 0o644 }); created = true;
    await hooks.afterTemp?.();
    if (replace) {
      await link(path, backup); backedUp = true; onBackup(backup);
      await hooks.afterBackup?.(backup);
      const current = await existing(backup);
      if (!current || hash(current) !== replace) throw new Error(`existing skill differs: ${path}`);
      await hooks.beforeRename?.(backup);
      const [live, saved] = await Promise.all([lstat(path).catch(() => null), lstat(backup)]);
      if (!live || live.ino !== saved.ino || live.dev !== saved.dev) throw new Error(`existing skill changed during approve: ${path}`);
      if (hash(await readFile(backup)) !== replace) throw new Error(`existing skill changed during approve: ${path}`);
      await hooks.beforeFinal?.();
      await rename(temp, path); created = false; onWritten();
    } else { await hooks.beforeFinal?.(); await link(temp, path); onWritten(); }
  } catch (error) {
    if (backedUp) throw Object.assign(new Error(`${error.message}; backup: ${backup}`), { code: error.code });
    throw error;
  } finally { if (created) await unlink(temp).catch(error => { if (error.code !== 'ENOENT') throw error; }); }
}
export async function applySkillInstall(plan, content, { onWritten = () => {}, onBackup = () => {}, hooks = {} } = {}) {
  const changed = (item, failure) => Object.assign(new Error(`existing skill differs: ${item.path}: ${failure.message}`), { code: 'SKILL_CHANGED' });
  async function contain(item, requireDirectory = false) {
    try {
      const roots = planRoots.get(item);
      if (!roots || item.action === 'blocked') throw new Error('blocked or invalid plan');
      const currentRoot = requireDirectory ? await realpath(roots.configuredRoot) : await targetRoot(roots.configuredRoot);
      if (currentRoot !== roots.resolvedRoot || await symlinked(roots.resolvedRoot, item.path)) throw new Error('symlinked path or target changed');
      if (requireDirectory && !(await lstat(dirname(item.path))).isDirectory()) throw new Error('invalid skill directory');
    } catch (failure) { throw changed(item, failure); }
  }
  async function check(item) {
    await contain(item);
    try {
      const current = await existing(item.path);
      if ((item.action === 'create' && current !== null)
          || (item.action === 'identical' && (!current || hash(current) !== hash(content)))
          || (item.action === 'replace' && (!current || hash(current) !== item.replace))) throw new Error('changed');
    } catch (failure) { throw changed(item, failure); }
  }
  for (const item of plan) await check(item);
  for (const item of plan) {
    await check(item);
    if (item.action === 'identical') continue;
    const { resolvedRoot } = planRoots.get(item);
    const physicalPath = join(resolvedRoot, basename(dirname(item.path)), basename(item.path));
    const installHooks = {
      afterMkdir: async () => { await hooks.afterMkdir?.(); await contain(item, true); },
      afterTemp: async () => { await hooks.afterTemp?.(); await contain(item, true); },
      afterBackup: hooks.afterBackup,
      beforeRename: async backup => { await hooks.beforeRename?.(backup); await contain(item, true); },
      beforeFinal: () => contain(item, true),
    };
    try {
      await installSkillFile(physicalPath, content, item.replace, () => onWritten(item.path), installHooks,
        backup => onBackup(join(dirname(item.path), basename(backup))));
    } catch (failure) {
      if (failure.code !== 'SKILL_CHANGED') await contain(item);
      throw failure;
    }
  }
}

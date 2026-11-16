// Offline, consented source consolidation. Stores are never deleted.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, copyFileSync, cpSync, existsSync, mkdtempSync, mkdirSync, openSync, readSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { fileURLToPath } from 'node:url';
import { ensureStoreParent, isIgnored, loadConfig, nativeEnvironment, resolveDirectoryIdentityAsync, safeEnvironment } from './project-binding.mjs';
import { loadRegistry, MAX_REGISTRY_BYTES, MAX_REGISTRY_ENTRIES, removeProjects, replaceProjects } from './project-registry.mjs';

const MAX_CHUNK_LINES = 50000, MAX_CHUNK_BYTES = 48 * 1024 * 1024;

const hash = project => createHash('sha256').update(project).digest('hex');
const store = (config, project) => join(config.dataDir, hash(project), 'store');
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;

function checkService() {
  const dir = process.env.XDG_RUNTIME_DIR || `/run/user/${process.getuid()}`;
  const result = spawnSync('systemctl', ['--user', 'show', 'lambda-memory.service', '--property=ActiveState', '--value'], {
    env: { ...safeEnvironment(), XDG_RUNTIME_DIR: dir, DBUS_SESSION_BUS_ADDRESS: process.env.DBUS_SESSION_BUS_ADDRESS || `unix:path=${dir}/bus` }, encoding: 'utf8', timeout: 5000,
  });
  if (result.error || result.status !== 0 || !['inactive', 'failed'].includes(result.stdout.trim())) throw new Error('authorized maintenance requires lambda-memory.service to be stopped');
}
function native(config, project, cwd, flag, inputFd, outputFd) {
  const result = spawnSync(config.nativeBin, [flag], {
    cwd, env: { ...safeEnvironment(), ...nativeEnvironment(config), LAMBDA_MEMORY_DATA_DIR: store(config, project),
      LAMBDA_MEMORY_OWNER: config.owner, LAMBDA_MEMORY_ACTOR: config.actor, LAMBDA_MEMORY_PROJECT: project,
      LAMBDA_MEMORY_TOKEN: config.token, LAMBDA_MEMORY_CWD: cwd },
    stdio: [inputFd ?? 'ignore', outputFd ?? 'pipe', 'ignore'], encoding: 'utf8', maxBuffer: 128 * 1024 * 1024,
  });
  if (result.error || result.status !== 0) throw new Error('native maintenance failed; do not automatically retry');
  if (outputFd !== undefined) return;
  const value = JSON.parse(result.stdout);
  if (!value || value.project !== project) throw new Error('native maintenance failed; do not automatically retry');
  return value;
}
async function plan(config) {
  const merges = [], drops = [], missing = [], changed_repos = []; let kept = 0;
  for (const entry of loadRegistry(config.dataDir)) {
    const { project, cwd } = entry;
    if (isIgnored(cwd, config.ignoreRoots)) { drops.push({ project, cwd }); continue; }
    const result = await resolveDirectoryIdentityAsync(cwd, { ignoreRoots: config.ignoreRoots });
    if (result.action === 'missing') { missing.push({ project, cwd }); continue; }
    if (result.action === 'drop') { drops.push({ project, cwd }); continue; }
    if (project.startsWith('repo:')) {
      if (result.binding.project !== project) changed_repos.push({ project, cwd, resolved: result.binding.project });
      else kept++;
      continue;
    }
    if (result.binding.project === project) { kept++; continue; }
    let sources = 0, curatable = 0;
    if (existsSync(store(config, project))) {
      const counts = native(config, project, cwd, '--inspect-sources');
      if (!Number.isSafeInteger(counts.sources) || counts.sources < 0 || !Number.isSafeInteger(counts.curatable) || counts.curatable < 0 || counts.curatable > counts.sources) throw new Error('native maintenance failed; do not automatically retry');
      ({ sources, curatable } = counts);
    }
    merges.push({ from_project: project, from_cwd: cwd, to_project: result.binding.project, to_cwd: result.binding.cwd, sources, curatable });
  }
  merges.sort((a, b) => compare(a.from_project, b.from_project));
  for (const list of [drops, missing, changed_repos]) list.sort((a, b) => compare(a.project, b.project));
  return { merges, drops, missing, changed_repos, kept, estimated_curation_calls: Math.ceil(merges.reduce((sum, item) => sum + item.curatable, 0) / 4), accept_charges: false };
}
async function readPlan() {
  const chunks = []; let size = 0;
  for await (const chunk of process.stdin) { size += chunk.length; if (size > 1048576) throw new Error('exact identity merge consent required'); chunks.push(chunk); }
  try { return JSON.parse(Buffer.concat(chunks).toString('utf8')); } catch { throw new Error('exact identity merge consent required'); }
}
function importMerge(config, merge, onImport, { chunkLines, chunkBytes }) {
  const total = { inserted: 0, duplicates: 0, conflicts: [], conflicts_total: 0, rejected: 0, remaining: 0 };
  if (!existsSync(store(config, merge.from_project))) return total;
  const dir = mkdtempSync(join(tmpdir(), 'lambda-identity-'));
  try {
    const file = join(dir, 'sources.jsonl'), output = openSync(file, 'wx', 0o600);
    try { native(config, merge.from_project, merge.from_cwd, '--export-sources', undefined, output); } finally { closeSync(output); }
    const source = openSync(file, 'r'), chunkFile = join(dir, 'chunk.jsonl');
    let chunk = openSync(chunkFile, 'wx', 0o600), bytes = 0, lines = 0, imports = 0;
    const flush = () => {
      closeSync(chunk); chunk = undefined;
      const input = openSync(chunkFile, 'r');
      let result;
      try { ensureStoreParent(store(config, merge.to_project)); onImport(); result = native(config, merge.to_project, merge.to_cwd, '--import-sources', input); }
      finally { closeSync(input); }
      if (!Number.isSafeInteger(result.inserted) || result.inserted < 0 || !Number.isSafeInteger(result.duplicates) || result.duplicates < 0 || !Number.isSafeInteger(result.remaining) || result.remaining < 0 || !Array.isArray(result.conflicts) || !result.conflicts.every(x => typeof x === 'string') || !Array.isArray(result.rejected) || !result.rejected.every(x => typeof x === 'string')) throw new Error('invalid native result');
      total.inserted += result.inserted; total.duplicates += result.duplicates; total.remaining += result.remaining;
      total.conflicts_total += result.conflicts.length; total.conflicts.push(...result.conflicts.slice(0, 32 - total.conflicts.length));
      total.rejected += result.rejected.length; imports++;
      chunk = openSync(chunkFile, 'w', 0o600); bytes = 0; lines = 0;
    };
    const append = line => {
      if (line.length > chunkBytes) throw new Error('source line exceeds chunk limit');
      if (lines && (lines === chunkLines || bytes + line.length > chunkBytes)) flush();
      writeFileSync(chunk, line); bytes += line.length; lines++;
    };
    try {
      const buffer = Buffer.alloc(64 * 1024); let pending = Buffer.alloc(0), count;
      while ((count = readSync(source, buffer, 0, buffer.length, null)) > 0) {
        const data = Buffer.concat([pending, buffer.subarray(0, count)]); let start = 0, end;
        while ((end = data.indexOf(10, start)) !== -1) { append(data.subarray(start, end + 1)); start = end + 1; }
        pending = Buffer.from(data.subarray(start));
        if (pending.length > chunkBytes) throw new Error('source line exceeds chunk limit');
      }
      if (pending.length) append(pending);
      if (lines || imports === 0) flush();
    } finally { closeSync(source); if (chunk !== undefined) closeSync(chunk); }
    return total;
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

// The registry must keep every identity that still holds sources: a target is recorded (and a merged
// source removed) in one atomic write, and an import never starts when the target could not be recorded.
function targetFits(config, merge) {
  const entries = loadRegistry(config.dataDir);
  if (entries.some(entry => entry.project === merge.to_project)) return true;
  const target = { project: merge.to_project, cwd: merge.to_cwd, last_seen_ms: Date.now() };
  return entries.length + 1 <= MAX_REGISTRY_ENTRIES && Buffer.byteLength(JSON.stringify({ projects: [target, ...entries] })) <= MAX_REGISTRY_BYTES;
}
export async function main({ chunkLines = MAX_CHUNK_LINES, chunkBytes = MAX_CHUNK_BYTES } = {}) {
  if (!Number.isSafeInteger(chunkLines) || chunkLines < 1 || chunkLines > MAX_CHUNK_LINES || !Number.isSafeInteger(chunkBytes) || chunkBytes < 1 || chunkBytes > MAX_CHUNK_BYTES) throw new Error('invalid chunk limits');
  const args = process.argv.slice(2);
  if (!(args.length === 1 && args[0] === 'inspect') && !(args.length === 3 && args[0] === 'apply' && args[1] === '--backup' && args[2] && !args[2].startsWith('--'))) throw new Error('usage: node runtime/merge-identities.mjs inspect | apply --backup <dir>');
  checkService();
  const config = loadConfig();
  if (args[0] === 'inspect') { console.log(JSON.stringify(await plan(config))); return; }
  const consent = await readPlan();
  if (!consent || typeof consent !== 'object' || Array.isArray(consent) || consent.accept_charges !== true || !isDeepStrictEqual({ ...consent, accept_charges: false }, await plan(config))) throw new Error('exact identity merge consent required');
  const backup = args[2];
  mkdirSync(backup, { mode: 0o700 });
  copyFileSync(join(config.dataDir, 'projects.json'), join(backup, 'projects.json'));
  for (const target of new Set(consent.merges.map(item => item.to_project))) {
    if (existsSync(store(config, target))) cpSync(join(config.dataDir, hash(target)), join(backup, hash(target)), { recursive: true, errorOnExist: true });
  }
  const outcomes = []; let failed = false, conflict = false;
  for (const merge of consent.merges) {
    const identity = { from_project: merge.from_project, to_project: merge.to_project };
    if (!targetFits(config, merge)) { outcomes.push({ ...identity, outcome: 'registry_full' }); conflict = true; continue; }
    let imported = false;
    try {
      const result = importMerge(config, merge, () => { imported = true; }, { chunkLines, chunkBytes });
      const outcome = result.conflicts_total ? 'conflict' : result.remaining > 0 ? 'partial' : 'merged';
      if (imported) await replaceProjects(config.dataDir, outcome === 'merged' ? [merge.from_project] : [], { project: merge.to_project, cwd: merge.to_cwd });
      else if (outcome === 'merged') await removeProjects(config.dataDir, [merge.from_project]);
      outcomes.push({ ...identity, outcome, inserted: result.inserted, duplicates: result.duplicates, conflicts: result.conflicts, conflicts_total: result.conflicts_total, rejected: result.rejected, remaining: result.remaining });
      if (outcome === 'conflict') conflict = true;
    } catch {
      if (imported) { try { await replaceProjects(config.dataDir, [], { project: merge.to_project, cwd: merge.to_cwd }); } catch { /* reported as failed */ } }
      outcomes.push({ ...identity, outcome: 'failed' }); failed = true; break;
    }
  }
  let dropped = 0;
  if (!failed && !conflict) dropped = await removeProjects(config.dataDir, consent.drops.map(item => item.project));
  console.log(JSON.stringify({ outcomes, dropped, backup }));
  if (failed) process.exitCode = 2;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch(error => {
    const allowed = ['authorized maintenance requires lambda-memory.service to be stopped', 'exact identity merge consent required', 'usage: node runtime/merge-identities.mjs inspect | apply --backup <dir>'];
    console.error(allowed.includes(error.message) ? error.message : 'identity maintenance failed'); process.exitCode = 2;
  });
}

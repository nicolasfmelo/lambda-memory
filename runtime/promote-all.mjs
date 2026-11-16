// Offline registry-wide promotion. The native process is the sole writer of each store.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, createReadStream, existsSync, mkdtempSync, openSync, rmSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { isDeepStrictEqual } from 'node:util';
import { fileURLToPath } from 'node:url';
import { copyId } from './cross-store.mjs';
import { ensureStoreParent, loadConfig, nativeEnvironment, safeEnvironment, userIdentity } from './project-binding.mjs';
import { loadRegistry } from './project-registry.mjs';

const MAX_CHUNK_LINES = 50000, MAX_CHUNK_BYTES = 48 * 1024 * 1024;
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const store = (config, project) => join(config.dataDir, createHash('sha256').update(project).digest('hex'), 'store');
const chunks = function* (items, size) { for (let i = 0; i < items.length; i += size) yield items.slice(i, i + size); };
function checkService() {
  const dir = process.env.XDG_RUNTIME_DIR || `/run/user/${process.getuid()}`;
  const result = spawnSync('systemctl', ['--user', 'show', 'lambda-memory.service', '--property=ActiveState', '--value'], {
    env: { ...safeEnvironment(), XDG_RUNTIME_DIR: dir, DBUS_SESSION_BUS_ADDRESS: process.env.DBUS_SESSION_BUS_ADDRESS || `unix:path=${dir}/bus` }, encoding: 'utf8', timeout: 5000,
  });
  if (result.error || result.status !== 0 || !['inactive', 'failed'].includes(result.stdout.trim())) throw new Error('authorized maintenance requires lambda-memory.service to be stopped');
}
function directory(config, entry) {
  try { if (statSync(entry.cwd).isDirectory()) return entry.cwd; } catch { /* Missing directories still have stores. */ }
  return config.dataDir;
}
function native(config, entry, flag, input, { apply = false, outputFd } = {}) {
  checkService();
  const cwd = directory(config, entry);
  ensureStoreParent(store(config, entry.project));
  const result = spawnSync(config.nativeBin, [flag, ...(apply ? ['--apply'] : [])], {
    cwd, env: { ...safeEnvironment(), ...(flag === '--promote-existing' ? nativeEnvironment(config) : { ...config.runtimeEnv, LAMBDA_MEMORY_MODE: 'local' }),
      LAMBDA_MEMORY_DATA_DIR: store(config, entry.project), LAMBDA_MEMORY_OWNER: config.owner, LAMBDA_MEMORY_ACTOR: config.actor,
      LAMBDA_MEMORY_PROJECT: entry.project, LAMBDA_MEMORY_TOKEN: config.token, LAMBDA_MEMORY_CWD: cwd },
    input, stdio: ['pipe', outputFd ?? 'pipe', 'ignore'], encoding: 'utf8', maxBuffer: 128 * 1024 * 1024,
  });
  if (result.error || result.status !== 0) throw new Error('native promotion maintenance failed; do not automatically retry');
  if (outputFd !== undefined) return;
  const value = JSON.parse(result.stdout);
  if (['--inspect-promotions', '--promote-existing', '--import-sources', '--retire-promoted'].includes(flag) && value.project !== entry.project) throw new Error('native project mismatch');
  return value;
}
function entries(config) {
  const projects = [], skipped = [];
  for (const entry of loadRegistry(config.dataDir)) {
    if (entry.project.startsWith('user:')) skipped.push({ project: entry.project, reason: 'user_store' });
    else if (!existsSync(store(config, entry.project))) skipped.push({ project: entry.project, reason: 'missing_store' });
    else projects.push(entry);
  }
  projects.sort((a,b) => compare(a.project, b.project));
  skipped.sort((a,b) => compare(a.project, b.project));
  return { projects, skipped };
}
function plan(config) {
  const { projects: stores, skipped } = entries(config);
  const projects = stores.map(entry => native(config, entry, '--inspect-promotions'));
  projects.sort((a,b) => a.batches - b.batches || compare(a.project,b.project));
  return { projects, skipped, total_batches: projects.reduce((n,p) => n + p.batches, 0), total_linked_sources: projects.reduce((n,p) => n + p.linked_sources, 0), accept_charges: false };
}
export function summarize(plan) {
  return [`Projects to classify (paid): ${plan.projects.length}, classifier batches: ${plan.total_batches}`,
    ...plan.projects.map(p => `  ${p.batches} batches  ${p.nodes.length} nodes  ${p.linked_sources} linked sources  ${p.project}`),
    `Skipped: ${plan.skipped.length}`,
    'Each batch is one paid Haiku call. Transfer is free; each forwarded source later costs user-store curation and embeddings.',
    `Linked sources (upper bound on forwarded sources): ${plan.total_linked_sources}. Retirement queues paid parent refreshes. There is no spending cap.`,
    `Consent: ${plan.accept_charges === true ? 'GIVEN' : 'not given (set accept_charges to true only after the user approves)'}`].join('\n');
}
async function readPlan() {
  const parts = []; let bytes = 0;
  for await (const part of process.stdin) { bytes += part.length; if (bytes > 1048576) throw new Error('promotion input bound'); parts.push(part); }
  return JSON.parse(Buffer.concat(parts).toString('utf8'));
}
function requireConsent(consent, expected) {
  if (!consent || consent.accept_charges !== true || !isDeepStrictEqual({ ...consent, accept_charges: false }, expected)) throw new Error('exact promotion consent required');
}
function validateIds(value) {
  if (!Array.isArray(value) || !value.every(id => typeof id === 'string')) throw new Error('invalid native ID list');
  return value;
}
async function transfer(config, { chunkLines, chunkBytes }) {
  const user = { project: userIdentity(config.owner), cwd: config.dataDir }, outcomes = [];
  const dir = mkdtempSync(join(config.dataDir, '.promotion-'));
  try {
    for (const entry of entries(config).projects) {
      const path = join(dir, 'forwards.jsonl'), outputFd = openSync(path, 'w', 0o600);
      try { native(config, entry, '--export-forwards', undefined, { outputFd }); } finally { closeSync(outputFd); }
      let lines = [], bytes = 0;
      const total = { project: entry.project, done: 0, failed: 0, remaining: 0 };
      const flush = () => {
        if (!lines.length) return;
        const result = native(config, user, '--import-sources', lines.map(l => l.line).join(''));
        const done = new Set([...validateIds(result.inserted_ids), ...validateIds(result.duplicate_ids)]);
        const failed = new Set([...validateIds(result.rejected), ...validateIds(result.conflicts)]);
        const remaining = new Set(validateIds(result.remaining_ids));
        const expected = new Set(lines.map(l => l.id));
        if ([...done, ...failed, ...remaining].some(id => !expected.has(id)) || [...expected].some(id => Number(done.has(id)) + Number(failed.has(id)) + Number(remaining.has(id)) !== 1)) throw new Error('invalid native import partition');
        const acks = lines.filter(l => !remaining.has(l.id));
        for (const batch of chunks(acks, 64)) {
          const request = { done: batch.filter(l => done.has(l.id)).map(l => l.forward), failed: batch.filter(l => failed.has(l.id)).map(l => l.forward) };
          const ack = native(config, entry, '--forward-done', JSON.stringify(request));
          total.done += ack.done; total.failed += ack.failed;
        }
        total.remaining += remaining.size;
        lines = []; bytes = 0;
      };
      const input = createReadStream(path);
      const reader = createInterface({ input, crlfDelay: Infinity });
      try {
        for await (const text of reader) {
          const row = JSON.parse(text), id = copyId(entry.project, row.source_id);
          const recapture = typeof row.forward_id === 'string' && row.forward_id.startsWith('recapture:') ? row.forward_id.slice('recapture:'.length) : '';
          const separator = recapture.indexOf(':');
          const validRecapture = separator > 0 && /^[0-9]+$/.test(recapture.slice(0, separator)) && recapture.slice(separator + 1) === row.source_id;
          if (typeof row.forward_id !== 'string' || (row.forward_id !== `capture:${row.source_id}` && !validRecapture) || !row.source) throw new Error('invalid capture forward');
          const line = `${JSON.stringify({ ...row.source, source: id, origin: `promote://${entry.project}/${row.source_id}` })}\n`;
          const size = Buffer.byteLength(line);
          if (size > chunkBytes) throw new Error('source line exceeds chunk limit');
          if (lines.length && (lines.length === chunkLines || bytes + size > chunkBytes)) flush();
          lines.push({ line, id, forward: row.forward_id }); bytes += size;
        }
        flush();
      } finally { reader.close(); input.destroy(); }
      outcomes.push(total);
    }
    return { outcomes };
  } finally { rmSync(dir, { recursive: true, force: true }); }
}
function retirementPlan(config) {
  const user = { project: userIdentity(config.owner), cwd: config.dataDir }, projects = [];
  for (const entry of entries(config).projects) {
    const listed = native(config, entry, '--retire-promoted');
    const state = new Map();
    const ids = [...new Set(listed.eligible.flatMap(n => n.sources.map(s => copyId(entry.project, s))))];
    for (const batch of chunks(ids, 10000)) {
      const result = native(config, user, '--inspect-copies', JSON.stringify(batch));
      for (const category of ['active', 'consumed', 'pending', 'missing']) {
        for (const id of validateIds(result[category])) {
          if (!batch.includes(id) || state.has(id)) throw new Error('invalid native copy partition');
          state.set(id, category);
        }
      }
      if (batch.some(id => !state.has(id))) throw new Error('incomplete native copy inspection');
    }
    const item = { project: entry.project, confirmed: [], waiting: [], incomplete: [], orphaned: [], blocked: [...new Set(validateIds(listed.blocked))] };
    for (const node of listed.eligible) {
      if (item.blocked.includes(node.node)) continue;
      const copies = node.sources.map(s => state.get(copyId(entry.project, s)));
      const category = copies.includes('pending') ? 'waiting' : copies.includes('missing') ? 'incomplete' : !copies.includes('active') ? 'orphaned' : 'confirmed';
      item[category].push(node.node);
    }
    for (const key of ['confirmed', 'waiting', 'incomplete', 'orphaned', 'blocked']) item[key].sort(compare);
    projects.push(item);
  }
  return { projects, accept_charges: false };
}
export async function main({ chunkLines = MAX_CHUNK_LINES, chunkBytes = MAX_CHUNK_BYTES } = {}) {
  if (!Number.isSafeInteger(chunkLines) || chunkLines < 1 || chunkLines > MAX_CHUNK_LINES || !Number.isSafeInteger(chunkBytes) || chunkBytes < 1 || chunkBytes > MAX_CHUNK_BYTES) throw new Error('invalid chunk limits');
  const [action, ...rest] = process.argv.slice(2);
  if (!['plan','summary','apply','transfer','retire-plan','retire'].includes(action) || (action === 'retire' ? rest.length !== 1 || rest[0] !== '--confirm' : rest.length !== 0)) throw new Error('usage: node runtime/promote-all.mjs plan | summary | apply | transfer | retire-plan | retire --confirm');
  checkService();
  if (action === 'summary') { console.log(summarize(await readPlan())); return; }
  const config = loadConfig();
  if (action === 'plan') { console.log(JSON.stringify(plan(config))); return; }
  if (action === 'transfer') { console.log(JSON.stringify(await transfer(config, { chunkLines, chunkBytes }))); return; }
  if (action === 'retire-plan') { console.log(JSON.stringify(retirementPlan(config))); return; }
  const consent = await readPlan();
  requireConsent(consent, action === 'apply' ? plan(config) : retirementPlan(config));
  const stores = entries(config).projects, outcomes = [];
  for (const item of consent.projects) {
    const entry = stores.find(e => e.project === item.project);
    try {
      const result = action === 'apply' ? native(config, entry, '--promote-existing', JSON.stringify({ ...item, accept_charges: true }))
        : native(config, entry, '--retire-promoted', JSON.stringify(item.confirmed), { apply: true });
      outcomes.push(result);
    } catch {
      outcomes.push({ project: item.project, outcome: 'failed' });
      console.log(JSON.stringify({ outcomes })); process.exitCode = 2; return;
    }
  }
  console.log(JSON.stringify({ outcomes }));
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch(error => { console.error(error.message); process.exitCode = 2; });
}

// Operator-only migration of registered stores; each native command verifies its own snapshot.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadConfig, resolveProjectBinding, safeEnvironment } from './project-binding.mjs';
import { loadRegistry } from './project-registry.mjs';

const recovery = fileURLToPath(new URL('./recover-pending.mjs', import.meta.url));
function checkService() {
  const dir = process.env.XDG_RUNTIME_DIR || `/run/user/${process.getuid()}`;
  const result = spawnSync('systemctl', ['--user', 'show', 'lambda-memory.service', '--property=ActiveState', '--value'], {
    env: { ...safeEnvironment(), XDG_RUNTIME_DIR: dir, DBUS_SESSION_BUS_ADDRESS: process.env.DBUS_SESSION_BUS_ADDRESS || `unix:path=${dir}/bus` }, encoding: 'utf8', timeout: 5000,
  });
  if (result.error || result.status !== 0 || !['inactive', 'failed'].includes(result.stdout.trim())) throw new Error('authorized maintenance requires lambda-memory.service to be stopped');
}
async function run(action, entry, input) {
  const child = spawn(process.execPath, [recovery, action], { env: { ...process.env, LAMBDA_MEMORY_CWD: entry.cwd }, stdio: ['pipe', 'pipe', 'pipe'] });
  let output = ''; child.stdout.setEncoding('utf8');
  child.stdout.on('data', chunk => { if (output.length < 1048576) output += chunk; });
  child.stderr.resume(); child.stdin.on('error', () => {}); child.stdin.end(input && JSON.stringify(input));
  const status = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
  if (status !== 0) throw new Error('project maintenance failed; do not automatically retry');
  const result = JSON.parse(output);
  if (result.project !== entry.project) throw new Error('project binding mismatch');
  return result;
}
async function readPlan() {
  let size = 0; const chunks = [];
  for await (const chunk of process.stdin) { size += chunk.length; if (size > 1048576) throw new Error('migration input bound'); chunks.push(chunk); }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}
/** A human-readable view of a plan, for the person who must approve the charges. */
export function summarize(plan) {
  const paid = plan.projects.filter(p => p.batches > 0);
  const lines = [`Projects to curate (paid): ${paid.length}, curation batches: ${plan.total_batches}`];
  for (const p of paid) lines.push(`  ${String(p.batches).padStart(4)} batches  ${String(p.sources.length).padStart(5)} sources  ${p.project}`);
  lines.push(`Projects switched to tree mode at no cost: ${plan.projects.length - paid.length}`);
  const reasons = {};
  for (const s of plan.skipped) reasons[s.reason] = (reasons[s.reason] ?? 0) + 1;
  lines.push(`Skipped: ${plan.skipped.length}${Object.keys(reasons).length ? ` (${Object.entries(reasons).map(([r, n]) => `${r}: ${n}`).join(', ')})` : ''}`);
  lines.push('Each batch is one paid Haiku call; refresh/split maintenance and node embeddings add some more. There is no spending cap.');
  lines.push(`Consent: ${plan.accept_charges === true ? 'GIVEN' : 'not given (set accept_charges to true only after the user approves)'}`);
  return lines.join('\n');
}
async function main() {
  const [action, ...rest] = process.argv.slice(2);
  const usage = 'usage: node runtime/migrate-all-trees.mjs plan [--only <project>]... [--exclude <project>]... | summary < plan.json | apply < approved.json';
  if (!['plan', 'summary', 'apply'].includes(action)) throw new Error(usage);
  if (action === 'summary') {
    if (rest.length) throw new Error(usage);
    console.log(summarize(await readPlan()));
    return;
  }
  const only = [], exclude = [];
  for (let i = 0; i < rest.length; i += 2) {
    const [flag, value] = [rest[i], rest[i + 1]];
    if (action !== 'plan' || !['--only', '--exclude'].includes(flag) || typeof value !== 'string' || !value) throw new Error(usage);
    (flag === '--only' ? only : exclude).push(value);
  }
  checkService();
  const entries = loadRegistry(loadConfig().dataDir);
  if (action === 'plan') {
    const projects = [], skipped = [];
    for (const entry of entries) {
      if (exclude.includes(entry.project)) { skipped.push({ project: entry.project, cwd: entry.cwd, reason: 'excluded' }); continue; }
      if (only.length && !only.includes(entry.project)) { skipped.push({ project: entry.project, cwd: entry.cwd, reason: 'not_selected' }); continue; }
      if (!existsSync(entry.cwd)) { skipped.push({ project: entry.project, cwd: entry.cwd, reason: 'missing_cwd' }); continue; }
      // An older identity for this directory (for example before a Git remote existed) has its own store
      // that this directory no longer binds to; report it instead of failing the whole plan.
      let current;
      try { current = resolveProjectBinding(entry.cwd).project; } catch { current = null; }
      if (current !== entry.project) { skipped.push({ project: entry.project, cwd: entry.cwd, reason: 'binding_changed' }); continue; }
      projects.push({ ...await run('inspect-tree', entry), cwd: entry.cwd });
    }
    // Cheapest projects first, so a provider problem shows up after as little paid work as possible.
    projects.sort((a, b) => a.batches - b.batches || a.project.localeCompare(b.project));
    console.log(JSON.stringify({ projects, skipped, total_batches: projects.reduce((n, p) => n + p.batches, 0), accept_charges: false }));
    return;
  }
  const plan = await readPlan();
  if (!plan || plan.accept_charges !== true || !Array.isArray(plan.projects) || !Array.isArray(plan.skipped) || !Number.isSafeInteger(plan.total_batches) || Object.keys(plan).length !== 4 || plan.projects.reduce((n,p) => n + p.batches, 0) !== plan.total_batches) throw new Error('exact tree migration consent required');
  const outcomes = [], skipped = [...plan.skipped];
  for (const item of plan.projects) {
    const { cwd, ...request } = item;
    const entry = entries.find(e => e.project === request.project && e.cwd === cwd);
    if (!entry) throw new Error('project registry mismatch');
    if (!existsSync(cwd)) { skipped.push({ project: entry.project, cwd, reason: 'missing_cwd' }); continue; }
    try {
      const result = await run('curate-existing', entry, { ...request, accept_charges: true });
      outcomes.push(result);
    } catch {
      outcomes.push({ project: entry.project, outcome: 'failed', reason: 'project maintenance failed' });
      console.log(JSON.stringify({ outcomes, skipped })); process.exitCode = 2; return;
    }
  }
  console.log(JSON.stringify({ outcomes, skipped }));
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch(error => { console.error(error.message); process.exitCode = 2; });
}

import { createHash, randomUUID } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { lstat, mkdir, rename, unlink, writeFile } from 'node:fs/promises';
import { createConnection } from 'node:net';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { frames, writeFrame, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES } from './framing.mjs';
import { loadConfig, resolveCallBindingAsync, userIdentity } from './project-binding.mjs';
import { skillSlug as slug, planSkillInstall, applySkillInstall } from './skill-install.mjs';
export { installSkillFile } from './skill-install.mjs';

const hash = content => createHash('sha256').update(content).digest('hex');
function argumentsOf(argv) {
  const [command, ...rest] = argv;
  if (!['list', 'show', 'approve', 'reject', 'export'].includes(command)) throw new Error('invalid command');
  let id, cwd, revision, draft, user = false, confirm = false;
  for (let i = 0; i < rest.length; i++) {
    const item = rest[i];
    if (item === '--user' && !user) user = true;
    else if (item === '--confirm' && !confirm) confirm = true;
    else if (item === '--cwd' && cwd === undefined && rest[i + 1] && !rest[i + 1].startsWith('--')) cwd = resolve(rest[++i]);
    else if (item === '--revision' && revision === undefined && rest[i + 1] && !rest[i + 1].startsWith('--')) revision = rest[++i];
    else if (item === '--draft' && draft === undefined) draft = rest[i + 1] && !rest[i + 1].startsWith('--') ? rest[++i] : '';
    else if (!item.startsWith('--') && id === undefined) id = item;
    else throw new Error('invalid arguments');
  }
  if ((['show', 'approve', 'reject'].includes(command) !== Boolean(id)) || (command !== 'approve' && (confirm || revision !== undefined || draft !== undefined))) throw new Error('invalid arguments');
  if (command === 'approve' && (!confirm || !revision || !/^[0-9a-f]{16}$/.test(draft || ''))) throw new Error('approve requires --revision <rev>, --draft <hash> and --confirm');
  return { command, id, cwd, revision, draft, user };
}
async function client(config, binding, user) {
  const socket = createConnection(config.socketPath);
  socket.on('error', () => {});
  try {
    await new Promise((ok, fail) => { socket.once('connect', ok); socket.once('error', fail); });
    const input = frames(socket, MAX_RESPONSE_BYTES);
    await writeFrame(socket, { token: config.token, operator: true, ...(user ? { store: 'user' } : { cwd: binding.cwd }) }, MAX_REQUEST_BYTES);
    const next = async () => { const frame = await input.next(); if (frame.done) throw new Error('service disconnected'); return JSON.parse(frame.value); };
    const hello = await next();
    if (!hello.ready || hello.project !== binding.project) throw new Error('service binding mismatch');
    let id = 0;
    const request = async (method, params) => {
      const current = ++id;
      await writeFrame(socket, { jsonrpc: '2.0', id: current, method, params }, MAX_REQUEST_BYTES);
      const response = await next();
      if (response.id !== current || response.error || !response.result) throw new Error(response.error?.message || 'service request failed');
      return response.result;
    };
    await request('initialize', { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'skill-candidates', version: '1' }, _meta: { lambdaMemoryToken: config.token } });
    await writeFrame(socket, { jsonrpc: '2.0', method: 'notifications/initialized' }, MAX_REQUEST_BYTES);
    return {
      async call(name, args) {
        const result = await request('tools/call', { name, arguments: { project: binding.project, ...args } });
        let body;
        try { body = JSON.parse(result.content.filter(part => part.type === 'text').map(part => part.text).join('')); } catch { throw new Error('invalid tool response'); }
        if (result.isError) throw new Error(body.error || 'tool failed');
        return body;
      },
      close() { socket.destroy(); },
    };
  } catch (error) { socket.destroy(); throw error; }
}
async function main() {
  const options = argumentsOf(process.argv.slice(2)), config = loadConfig();
  const binding = options.user ? { project: userIdentity(config.owner) }
    : await resolveCallBindingAsync(options.cwd || process.cwd(), { ignoreRoots: config.ignoreRoots });
  if (!binding) throw new Error('ignored directory');
  const scope = options.user ? 'user' : 'project';
  const service = await client(config, binding, options.user);
  const written = [];
  try {
    const list = async () => {
      const result = await service.call('memory_skill_candidates', {});
      if (!Array.isArray(result.candidates)) throw new Error('invalid candidates response');
      return result.candidates;
    };
    const detail = candidate_id => service.call('memory_skill_candidate', { candidate_id });
    const decide = (candidate_id, decision, revision_id, extra = {}) => service.call('memory_skill_decide', { candidate_id, decision, revision_id, ...extra });
    if (options.command === 'list') {
      const candidates = (await list()).map(row => row.status === 'stale' && row.install_paths?.length ? { ...row, outdated: true } : row);
      console.log(JSON.stringify({ scope, project: binding.project, candidates }));
    } else if (options.command === 'reject') {
      const row = await detail(options.id);
      if (typeof row.revision_id !== 'string') throw new Error('invalid candidate');
      await decide(options.id, 'rejected', row.revision_id);
    }
    else if (options.command === 'show') {
      const row = await detail(options.id);
      if (typeof row.draft !== 'string' || typeof row.revision_id !== 'string') throw new Error('invalid candidate');
      process.stdout.write(`${row.draft}${row.draft.endsWith('\n') ? '' : '\n'}revision: ${row.revision_id}\ndraft: ${hash(row.draft).slice(0, 16)}\n`);
    } else if (options.command === 'export') {
      const base = join(process.env.HOME || homedir(), '.local/share/lambda-memory/skill-candidates');
      for (const candidate of await list()) {
        if (candidate.status !== 'drafted') continue;
        const row = await detail(candidate.candidate_id);
        const directory = join(base, scope, slug(row)), path = join(directory, 'SKILL.md');
        if (typeof row.draft !== 'string') throw new Error('invalid candidate');
        for (const dir of [base, join(base, scope), directory]) {
          await mkdir(dir, { recursive: true, mode: 0o755 });
          if (!(await lstat(dir)).isDirectory()) throw new Error('invalid export directory');
        }
        const temp = join(directory, `.lambda-skill-${randomUUID()}.tmp`);
        let created = false;
        try { await writeFile(temp, row.draft, { flag: 'wx', mode: 0o644 }); created = true; await rename(temp, path); created = false; }
        finally { if (created) await unlink(temp); }
        console.log(path);
      }
    } else {
      const row = await detail(options.id);
      if (!['drafted', 'approved'].includes(row.status)) throw new Error('candidate is not drafted or approved');
      if (row.revision_id !== options.revision) throw new Error('revision does not match');
      if (typeof row.draft !== 'string') throw new Error('invalid candidate');
      const content = Buffer.from(row.draft), newHash = hash(content);
      if (newHash.slice(0, 16) !== options.draft) throw new Error('draft changed; run show again');
      const plans = await planSkillInstall({ row, scope, targets: config.skillTargets[scope], projectRoot: binding.cwd });
      const blocked = plans.find(plan => plan.action === 'blocked');
      if (blocked) throw new Error(`existing skill differs: ${blocked.path}: ${blocked.reason}`);
      const paths = plans.map(plan => plan.path);
      await applySkillInstall(plans, content, { onWritten: path => written.push(path), onBackup: backup => console.log(`backup: ${backup}`) });
      if (row.status !== 'approved') await decide(options.id, 'approved', options.revision);
      await decide(options.id, 'installed', options.revision, { install_paths: paths, install_hashes: paths.map(() => newHash) });
    }
  } catch (error) {
    if (written.length) throw new Error(`${error.message}; written: ${written.join(', ')}`);
    throw error;
  } finally { service.close(); }
}
const entry = process.argv[1] ? (() => { try { return realpathSync(process.argv[1]); } catch { return ''; } })() : '';
if (entry === realpathSync(fileURLToPath(import.meta.url))) {
  try { await main(); }
  catch (error) { console.error(String(error.message || 'operation failed').replace(/[\r\n\u0000-\u001f\u007f-\u009f]+/g, ' ')); process.exitCode = 2; }
}

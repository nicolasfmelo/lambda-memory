import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { test } from 'node:test';
import { startupReason } from '../runtime/local-service.mjs';
import { VERSION } from '../runtime/version.mjs';
import { BRIEF_SIDE_DEADLINE_MS, copyId, RECALL_SIDE_DEADLINE_MS } from '../runtime/cross-store.mjs';

const TOKEN = 'lambda-memory-service-test-token-32b!';
const TOOL_NAMES = ['memory_status', 'memory_timeline', 'memory_timeline_write', 'memory_crash', 'memory_capture', 'memory_propose', 'memory_purge'];
const SERVICE = resolve('runtime/local-service.mjs');

const FAKE_NATIVE = resolve('tests/support/fake-native.cjs');

function fixture() {
  const root = mkdtempSync(join(tmpdir(), 'lm-svc-'));
  const native = FAKE_NATIVE;
  chmodSync(native, 0o755);
  const config = join(root, 'runtime.json');
  writeFileSync(config, JSON.stringify({
    nativeBin: native, bridgeBin: native, dataDir: join(root, 'data'), socketPath: join(root, 'run', 's.sock'),
    owner: 'owner', actor: 'actor', token: TOKEN,
  }), { mode: 0o600 });
  return { root, config, socketPath: join(root, 'run', 's.sock'), data: join(root, 'data') };
}

async function startService(t, configure) {
  const paths = fixture();
  paths.log = join(paths.root, 'native.log');
  let serviceEntry = SERVICE;
  if (configure) {
    const { env = {}, failUser = false, beforeNative = '', discoverChildren } = configure(paths) || {};
    const wrapper = join(paths.root, 'native.cjs');
    writeFileSync(wrapper, `#!/usr/bin/env node\nObject.assign(process.env, ${JSON.stringify({ FAKE_NATIVE_LOG: paths.log, ...env })});\n${failUser ? "if (process.env.LAMBDA_MEMORY_PROJECT.startsWith('user:')) process.exit(3);" : ''}\n${beforeNative}\nrequire(${JSON.stringify(FAKE_NATIVE)});\n`, { mode: 0o755 });
    const config = JSON.parse(readFileSync(paths.config, 'utf8'));
    writeFileSync(paths.config, JSON.stringify({ ...config, nativeBin: wrapper, ignoreRoots: [] }));
    if (discoverChildren) {
      serviceEntry = join(paths.root, 'service.mjs');
      writeFileSync(serviceEntry, `import { serve } from ${JSON.stringify(SERVICE)};
import { loadConfig } from ${JSON.stringify(resolve('runtime/project-binding.mjs'))};
import { workspaceChildren } from ${JSON.stringify(resolve('runtime/cross-store.mjs'))};
import { appendFileSync } from 'node:fs';
await serve(loadConfig(), { discoverChildren: ${discoverChildren} });\n`);
    }
  }
  const service = spawn(process.execPath, [serviceEntry], {
    env: { ...process.env, LAMBDA_MEMORY_CONFIG: paths.config, NOTIFY_SOCKET: '' }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  t.after(async () => {
    service.kill('SIGTERM');
    await new Promise((done) => (service.exitCode !== null ? done() : service.once('exit', done)));
    rmSync(paths.root, { recursive: true, force: true });
  });
  await new Promise((ready, failed) => {
    service.stdout.on('data', (chunk) => { if (String(chunk).includes('ready')) ready(); });
    service.once('exit', (code) => failed(new Error(`service exited ${code}`)));
  });
  return paths;
}

function connect(socketPath, cwd, extra = {}) {
  const socket = createConnection(socketPath);
  const lines = createInterface({ input: socket })[Symbol.asyncIterator]();
  let id = 0;
  const send = (message) => socket.write(`${JSON.stringify(message)}\n`);
  const next = async () => JSON.parse((await lines.next()).value);
  return {
    socket,
    async hello() { send({ cwd, token: TOKEN, ...extra }); return next(); },
    async request(method, params) { id += 1; send({ jsonrpc: '2.0', id, method, params }); return next(); },
    notify(method) { send({ jsonrpc: '2.0', method }); },
  };
}

async function ready(client) {
  assert.equal((await client.hello()).ready, true);
  const init = await client.request('initialize', {
    protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'test', version: '0' }, _meta: { lambdaMemoryToken: TOKEN },
  });
  assert.equal(init.result.serverInfo.name, 'lambda-memory');
  assert.equal(init.result.serverInfo.version, VERSION);
  client.notify('notifications/initialized');
}

test('tools/list restarts a crashed native worker instead of listing nothing', async (t) => {
  const { root, socketPath } = await startService(t);
  const client = connect(socketPath, root);
  await ready(client);
  assert.deepEqual((await client.request('tools/list', {})).result.tools.map((tool) => tool.name), TOOL_NAMES);
  const crashed = await client.request('tools/call', { name: 'memory_crash', arguments: {} });
  assert.match(crashed.error.message, /not confirmed/);
  assert.deepEqual((await client.request('tools/list', {})).result.tools.map((tool) => tool.name), TOOL_NAMES);
  assert.equal((await client.request('tools/call', { name: 'memory_unknown', arguments: {} })).error.code, -32602);
  client.socket.end();
});

test('the client socket closes promptly while the last worker shuts down', async (t) => {
  const { root, socketPath } = await startService(t);
  const client = connect(socketPath, root);
  await ready(client);
  await client.request('tools/list', {});
  const started = Date.now();
  const closed = new Promise((done) => client.socket.once('end', done));
  client.socket.end();
  await closed;
  assert.ok(Date.now() - started < 500, 'socket closed only after native shutdown');
});

test('the daemon records the project in the registry after a hello', async (t) => {
  const { root, socketPath, data } = await startService(t);
  const client = connect(socketPath, root);
  await ready(client);
  const path = join(data, 'projects.json');
  for (let i = 0; i < 50 && !existsSync(path); i += 1) await new Promise((done) => setTimeout(done, 20));
  const { projects } = JSON.parse(readFileSync(path, 'utf8'));
  assert.equal(projects.length, 1);
  assert.equal(projects[0].cwd, root);
  client.socket.end();
});

test('the mutating flag comes from readOnlyHint, and startWeb receives the seam', async (t) => {
  const paths = fixture();
  t.after(() => rmSync(paths.root, { recursive: true, force: true }));
  const { serve } = await import('../runtime/local-service.mjs');
  const { loadConfig } = await import('../runtime/project-binding.mjs');
  const calls = [];
  let seam, stopped = false;
  const done = serve(loadConfig({ LAMBDA_MEMORY_CONFIG: paths.config }), {
    startWeb(context) {
      seam = context;
      return { stop() { stopped = true; } };
    },
  });
  for (let i = 0; i < 100 && !seam; i += 1) await new Promise((ok) => setTimeout(ok, 20));
  assert.deepEqual(Object.keys(seam).sort(), ['acquire', 'config', 'registry', 'release', 'workers']);
  const client = connect(paths.socketPath, paths.root);
  await ready(client);
  const [worker] = seam.workers.values();
  const original = worker.client.callTool.bind(worker.client);
  worker.client.callTool = (name, args, options) => { calls.push([name, options.mutating]); return original(name, args, options); };
  const echoed = await client.request('tools/call', { name: 'memory_timeline_write', arguments: { a: 1 } });
  assert.deepEqual(JSON.parse(echoed.result.content[0].text), { name: 'memory_timeline_write', arguments: { a: 1 } });
  await client.request('tools/call', { name: 'memory_timeline', arguments: {} });
  await client.request('tools/call', { name: 'memory_status', arguments: {} });
  assert.deepEqual(calls, [['memory_timeline_write', true], ['memory_timeline', false], ['memory_status', false]]);
  client.socket.end();
  process.emit('SIGTERM');
  await done;
  assert.equal(stopped, true);
});

test('an argument rejection by the native validator is reported as -32602, not as an unconfirmed write', async (t) => {
  const { root, socketPath } = await startService(t);
  const client = connect(socketPath, root);
  await ready(client);
  const rejected = await client.request('tools/call', { name: 'memory_status', arguments: { rpc_error_code: -32602 } });
  assert.equal(rejected.error.code, -32602);
  assert.match(rejected.error.message, /invalid tool arguments; nothing was written/);
  const other = await client.request('tools/call', { name: 'memory_status', arguments: { rpc_error_code: -32603 } });
  assert.match(other.error.message, /not confirmed/);
  client.socket.end();
});

test('startup failures name the cause without echoing file contents', () => {
  assert.match(startupReason(new Error('private configuration required')), /mode 0600/);
  assert.match(startupReason(new Error('socket path bound')), /100 bytes/);
  assert.match(startupReason(Object.assign(new Error('x'), { code: 'ENOENT', path: '/tmp/none.json' })), /missing file: \/tmp\/none\.json/);
  const leaked = (() => { try { JSON.parse('{"token":"SECRET-VALUE'); } catch (e) { return e; } })();
  assert.equal(startupReason(leaked), 'configuration is not valid JSON');
  assert.ok(!startupReason(leaked).includes('SECRET'));
});

const projectRecall = { context: 'Project context', mode: 'tree', truncated: false, hits: [{ project: 'p', node: 'project-node' }] };
const userRecall = { context: 'User context', mode: 'tree', truncated: false, hits: [{ project: 'user:owner', node: 'user-node' }] };
const calls = paths => existsSync(paths.log) ? readFileSync(paths.log, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)) : [];
const call = (client, name, args = {}) => client.request('tools/call', { name, arguments: args });
const body = response => JSON.parse(response.result.content[0].text);
const recallEnv = root => ({ FAKE_NATIVE_RECALL_BY_PROJECT: JSON.stringify({ [`path:${root}`]: projectRecall, 'user:owner': userRecall }) });
async function waitFor(check) {
  for (let i = 0; i < 150; i++) {
    const value = check();
    if (value) return value;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.fail('condition did not become true');
}

test('capture with scope user goes to the user worker', async t => {
  const paths = await startService(t, () => ({}));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  await call(client, 'memory_capture', { project: `path:${paths.root}`, source: 'explicit', content: 'Preference', scope: 'user' });
  const capture = calls(paths).find(entry => entry.name === 'memory_capture');
  assert.equal(capture.project, 'user:owner');
  assert.equal(capture.arguments.project, 'user:owner');
  assert.equal(Object.hasOwn(capture.arguments, 'scope'), false);
  const tools = (await client.request('tools/list', {})).result.tools;
  assert.deepEqual(tools.find(tool => tool.name === 'memory_capture').inputSchema.properties.scope, { type: 'string', enum: ['project', 'user'] });
  await call(client, 'memory_capture', { project: `path:${paths.root}`, scope: 'project', source: 'local' });
  assert.equal(calls(paths).at(-1).project, `path:${paths.root}`);
  assert.equal(Object.hasOwn(calls(paths).at(-1).arguments, 'scope'), false);
  client.socket.end();
});

test('explicit project scope stays on the project worker even when naming the user store', async t => {
  const paths = await startService(t, () => ({}));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const args = { project: 'user:owner', scope: 'project', source: 'mismatched-project' };
  await call(client, 'memory_capture', args);
  const capture = calls(paths).find(entry => entry.name === 'memory_capture');
  assert.equal(capture.project, `path:${paths.root}`);
  assert.deepEqual(capture.arguments, { project: 'user:owner', source: 'mismatched-project' });
  client.socket.end();
});

test('invalid scope is rejected', async t => {
  const paths = await startService(t, () => ({}));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  for (const scope of ['global', null, 1]) assert.equal((await call(client, 'memory_capture', { scope })).error.code, -32602);
  assert.equal(calls(paths).some(entry => entry.name === 'memory_capture'), false);
  client.socket.end();
});

test('recall fans out to the user store and merges', async t => {
  const paths = await startService(t, ({ root }) => ({ env: recallEnv(root) }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const args = { projects: [`path:${paths.root}`], task: 'question', budget_bytes: 1024, exclude: ['old-node'] };
  const merged = body(await call(client, 'memory_recall', args));
  assert.equal(merged.context, 'Project context\n\nUser-level memory:\nUser context');
  assert.deepEqual(merged.hits.map(hit => [hit.node, hit.scope]), [['project-node', 'project'], ['user-node', 'user']]);
  const userCall = calls(paths).find(entry => entry.project === 'user:owner' && entry.name === 'memory_recall');
  assert.deepEqual(userCall.arguments, { ...args, projects: ['user:owner'] });
  client.socket.end();
});

test('recall returns the project result when the user worker fails', async t => {
  const paths = await startService(t, ({ root }) => ({ env: recallEnv(root), failUser: true }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const merged = body(await call(client, 'memory_recall', { task: 'question' }));
  assert.equal(merged.context, projectRecall.context);
  assert.deepEqual(merged.hits, projectRecall.hits.map(hit => ({ ...hit, scope: 'project' })));
  client.socket.end();
});

test('brief includes the user roots', async t => {
  const paths = await startService(t, ({ root }) => ({ env: { FAKE_NATIVE_BRIEF_BY_PROJECT: JSON.stringify({
    [`path:${root}`]: { project: `path:${root}`, mode: 'tree', roots: ['decisions'], truncated: false },
    'user:owner': { project: 'user:owner', mode: 'tree', roots: ['preferences', 'working_rules'], truncated: false },
  }) } }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const merged = body(await call(client, 'memory_brief', { project: `path:${paths.root}` }));
  assert.deepEqual(merged.roots, ['decisions']);
  assert.deepEqual(merged.user, { roots: ['preferences', 'working_rules'] });
  client.socket.end();
});

test('service-only tools are hidden and refused', async t => {
  const paths = await startService(t, () => ({ env: { FAKE_NATIVE_FORWARDS: '{"forwards":[]}',
    FAKE_NATIVE_EXTRA_TOOLS: 'memory_skill_candidates,memory_skill_candidate,memory_skill_decide' } }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const tools = (await client.request('tools/list', {})).result.tools.map(tool => tool.name);
  assert.equal(tools.includes('memory_forwards'), false);
  assert.equal(tools.includes('memory_forward_done'), false);
  for (const name of ['memory_forwards', 'memory_forward_done', 'memory_skill_candidates', 'memory_skill_candidate', 'memory_skill_decide']) {
    assert.equal(tools.includes(name), false);
    const response = await call(client, name, { project: 'user:owner' });
    assert.equal(response.error.code, -32602);
    assert.equal(response.error.message, 'unknown tool');
  }
  client.socket.end();
});

test('operator connections may call skill tools; plain clients may not', async t => {
  const paths = await startService(t, () => ({ env: { FAKE_NATIVE_EXTRA_TOOLS: 'memory_skill_candidates,memory_skill_candidate,memory_skill_decide' } }));
  const operator = connect(paths.socketPath, paths.root, { operator: true }); await ready(operator);
  const names = (await operator.request('tools/list', {})).result.tools.map(tool => tool.name);
  for (const name of ['memory_skill_candidates', 'memory_skill_candidate', 'memory_skill_decide']) {
    assert.ok(names.includes(name));
    assert.ok((await call(operator, name, { project: 'user:owner' })).result);
    assert.equal(calls(paths).at(-1).project, 'user:owner');
  }
  const plain = connect(paths.socketPath, paths.root); await ready(plain);
  assert.equal((await call(plain, 'memory_skill_candidates', {})).error.code, -32602);
  operator.socket.end(); plain.socket.end();
});

test('forward tools stay refused for operators', async t => {
  const paths = await startService(t, () => ({ env: { FAKE_NATIVE_FORWARDS: '{"forwards":[]}' } }));
  const client = connect(paths.socketPath, paths.root, { operator: true }); await ready(client);
  const names = (await client.request('tools/list', {})).result.tools.map(tool => tool.name);
  for (const name of ['memory_forwards', 'memory_forward_done']) {
    assert.equal(names.includes(name), false); assert.equal((await call(client, name)).error.code, -32602);
  }
  client.socket.end();
});

test('the user worker survives the call that used it', async t => {
  const paths = await startService(t, ({ root }) => ({ env: recallEnv(root) }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  await call(client, 'memory_recall', { task: 'first' });
  const first = calls(paths).find(entry => entry.project === 'user:owner');
  await new Promise(resolve => setTimeout(resolve, 1100));
  await call(client, 'memory_recall', { task: 'second' });
  const userCalls = calls(paths).filter(entry => entry.project === 'user:owner');
  assert.equal(userCalls.length, 2);
  assert.equal(userCalls[1].pid, first.pid);
  client.socket.end();
  await waitFor(() => { try { process.kill(first.pid, 0); return false; } catch { return true; } });
});

const promotion = { forward_id: 'capture:source-1', action: 'capture', source_id: 'source-1', source: {
  source: 'source-1', origin: 'exchange://original', recorded_at_ms: 123456, kind: 'decision', outcome: 'unknown', content: 'A durable preference',
} };

test('hello does not wait for background user startup', async t => {
  const paths = await startService(t, ({ root }) => ({ env: recallEnv(root),
    beforeNative: "if (process.env.LAMBDA_MEMORY_PROJECT.startsWith('user:')) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 2000);",
  }));
  const client = connect(paths.socketPath, paths.root);
  const start = Date.now();
  await ready(client);
  assert.ok(Date.now() - start < 1200, 'hello waited for the user worker');
  const recalled = body(await call(client, 'memory_recall', { task: 'question' }));
  assert.deepEqual(recalled.hits.map(hit => hit.scope), ['project', 'user']);
  client.socket.end();
});

test('a side call retries a failed background user lease', async t => {
  const paths = await startService(t, ({ root }) => ({ env: recallEnv(root),
    beforeNative: `if (process.env.LAMBDA_MEMORY_PROJECT.startsWith('user:')) {
      const fs = require('node:fs'), marker = ${JSON.stringify(join(root, 'user-attempted'))};
      if (!fs.existsSync(marker)) { fs.writeFileSync(marker, 'attempted'); process.exit(3); }
    }`,
  }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  await waitFor(() => existsSync(join(paths.root, 'user-attempted')));
  await new Promise(resolve => setTimeout(resolve, 1100));
  const recalled = body(await call(client, 'memory_recall', { task: 'first' }));
  assert.deepEqual(recalled.hits.map(hit => hit.scope), ['project', 'user']);
  const first = calls(paths).find(entry => entry.project === 'user:owner');
  await new Promise(resolve => setTimeout(resolve, 1100));
  await call(client, 'memory_recall', { task: 'second' });
  assert.equal(calls(paths).filter(entry => entry.project === 'user:owner').at(-1).pid, first.pid);
  client.socket.end();
});

test('permanent forward failures are marked failed', async t => {
  const paths = await startService(t, () => ({ env: {
    FAKE_NATIVE_FORWARDS: JSON.stringify({ forwards: [promotion] }),
    FAKE_NATIVE_ERROR_BY_PROJECT: JSON.stringify({ 'user:owner': 'invalid runtime input: rejected content' }),
  } }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const ack = await waitFor(() => calls(paths).find(entry => entry.name === 'memory_forward_done'));
  assert.deepEqual(ack.arguments, { project: `path:${paths.root}`, forwards: [], failed: ['capture:source-1'] });
  client.socket.end();
});

test('drain capacity backoff survives project worker replacement', async t => {
  const paths = await startService(t, () => ({ env: {
    FAKE_NATIVE_FORWARDS: JSON.stringify({ forwards: [promotion] }),
    FAKE_NATIVE_ERROR_BY_PROJECT: JSON.stringify({ 'user:owner': 'pending capture capacity reached' }),
  } }));
  const first = connect(paths.socketPath, paths.root); await ready(first);
  await waitFor(() => calls(paths).find(entry => entry.name === 'memory_capture'));
  // Let the receipt settle before disposing the project worker that started the drain.
  await new Promise(resolve => setTimeout(resolve, 100));
  const projectPid = calls(paths).find(entry => entry.name === 'memory_forwards').pid;
  first.socket.end();
  await waitFor(() => { try { process.kill(projectPid, 0); return false; } catch { return true; } });
  const second = connect(paths.socketPath, paths.root); await ready(second);
  await call(second, 'memory_status');
  const replacement = calls(paths).find(entry => entry.name === 'memory_status');
  assert.notEqual(replacement.pid, projectPid);
  await new Promise(resolve => setTimeout(resolve, 100));
  assert.equal(calls(paths).filter(entry => entry.name === 'memory_forwards').length, 1);
  assert.equal(calls(paths).filter(entry => entry.name === 'memory_capture').length, 1);
  second.socket.end();
});

test('a slow side does not block the merge', async t => {
  const paths = await startService(t, ({ root }) => ({ env: { ...recallEnv(root),
    FAKE_NATIVE_DELAY_BY_PROJECT: JSON.stringify({ 'user:owner': RECALL_SIDE_DEADLINE_MS + 1500 }),
  } }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const start = Date.now();
  const merged = body(await call(client, 'memory_recall', { task: 'question' }));
  const elapsed = Date.now() - start;
  assert.equal(merged.context, projectRecall.context);
  assert.deepEqual(merged.hits.map(hit => hit.scope), ['project']);
  assert.ok(elapsed < RECALL_SIDE_DEADLINE_MS + 500, `slow side delayed reply: ${elapsed}ms`);
  client.socket.end();
});

test('forwards are drained into the user store', async t => {
  const purge = { forward_id: 'purge:source-2', action: 'purge', source_id: 'source-2', source: null };
  const paths = await startService(t, () => ({ env: { FAKE_NATIVE_FORWARDS: JSON.stringify({ forwards: [promotion, purge] }) } }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const ack = await waitFor(() => calls(paths).find(entry => entry.name === 'memory_forward_done'));
  const project = `path:${paths.root}`;
  const capture = calls(paths).find(entry => entry.name === 'memory_capture');
  assert.equal(capture.project, 'user:owner');
  assert.deepEqual(capture.arguments, { project: 'user:owner', source: copyId(project, 'source-1'), kind: 'decision', outcome: 'unknown',
    origin: `promote://${project}/source-1`, content: promotion.source.content, recorded_at_ms: 123456 });
  const deletion = calls(paths).find(entry => entry.name === 'memory_purge');
  assert.equal(deletion.project, 'user:owner');
  assert.deepEqual(deletion.arguments, { project: 'user:owner', sources: [copyId(project, 'source-2')] });
  assert.equal(ack.project, project);
  assert.deepEqual(ack.arguments, { project, forwards: ['capture:source-1', 'purge:source-2'] });
  client.socket.end();
});

test('calls naming the user project are routed to the user worker', async t => {
  const paths = await startService(t, ({ root }) => ({ env: { ...recallEnv(root), FAKE_NATIVE_EXTRA_TOOLS: 'memory_node' } }));
  const client = connect(paths.socketPath, paths.root); await ready(client);
  await call(client, 'memory_node', { project: 'user:owner', node: 'user-node' });
  assert.equal(calls(paths).at(-1).project, 'user:owner');
  await call(client, 'memory_capture', { project: 'user:owner', source: 'explicit' });
  assert.equal(calls(paths).at(-1).project, 'user:owner');
  const recalled = body(await call(client, 'memory_recall', { projects: ['user:owner'], task: 'question' }));
  assert.deepEqual(recalled, userRecall);
  assert.equal(calls(paths).filter(entry => entry.name === 'memory_recall').length, 1);
  client.socket.end();
});

function registerChild(paths, project) {
  const cwd = join(paths.root, 'child'); mkdirSync(cwd); mkdirSync(paths.data, { mode: 0o700 });
  writeFileSync(join(paths.data, 'projects.json'), JSON.stringify({ projects: [{ project, cwd, last_seen_ms: 1 }] }), { mode: 0o600 });
  return cwd;
}

test('workspace discovery shares the recall and brief side deadlines', { timeout: 12000 }, async t => {
  const paths = await startService(t, paths => {
    registerChild(paths, 'repo:example.test/child');
    return { env: { ...recallEnv(paths.root), FAKE_NATIVE_BRIEF_BY_PROJECT: JSON.stringify({
      [`path:${paths.root}`]: { roots: ['decisions'] }, 'user:owner': { roots: ['preferences'] },
    }) }, discoverChildren: `async () => {
      appendFileSync(${JSON.stringify(join(paths.root, 'discoveries.log'))}, 'discovery\\n');
      await new Promise(resolve => setTimeout(resolve, ${RECALL_SIDE_DEADLINE_MS + BRIEF_SIDE_DEADLINE_MS + 800}));
      return [];
    }` };
  });
  const client = connect(paths.socketPath, paths.root); await ready(client);
  for (const [name, args, deadline] of [
    ['memory_recall', { task: 'question' }, RECALL_SIDE_DEADLINE_MS],
    ['memory_brief', { project: `path:${paths.root}` }, BRIEF_SIDE_DEADLINE_MS],
  ]) {
    const started = Date.now();
    const merged = body(await call(client, name, args));
    const elapsed = Date.now() - started;
    assert.ok(elapsed < deadline + 500, `${name} exceeded the shared side deadline: ${elapsed}ms`);
    assert.ok(elapsed >= deadline - 100, 'the injected discovery did not run');
    assert.equal(merged.workspace, undefined);
    assert.equal(calls(paths).some(entry => entry.project === 'repo:example.test/child'), false);
  }
  assert.equal(readFileSync(join(paths.root, 'discoveries.log'), 'utf8'), 'discovery\n');
  client.socket.end();
});

test('workspace discoveries are cached per project across recall and brief', async t => {
  const child = 'repo:example.test/child';
  const paths = await startService(t, paths => {
    registerChild(paths, child);
    return { env: { ...recallEnv(paths.root), FAKE_NATIVE_BRIEF_BY_PROJECT: JSON.stringify({
      [`path:${paths.root}`]: { roots: ['decisions'] }, 'user:owner': { roots: ['preferences'] }, [child]: { roots: ['architecture'] },
    }) }, discoverChildren: `async (...args) => {
      appendFileSync(${JSON.stringify(join(paths.root, 'discoveries.log'))}, 'discovery\\n');
      return workspaceChildren(...args);
    }` };
  });
  const client = connect(paths.socketPath, paths.root); await ready(client);
  await call(client, 'memory_recall', { task: 'first' });
  // A corrupt registry must not invalidate the successful discovery still within its 60 s TTL.
  writeFileSync(join(paths.data, 'projects.json'), 'invalid registry');
  const brief = body(await call(client, 'memory_brief', { project: `path:${paths.root}` }));
  assert.deepEqual(brief.workspace, [{ project: child, name: 'child', roots: ['architecture'] }]);
  assert.equal(readFileSync(join(paths.root, 'discoveries.log'), 'utf8'), 'discovery\n');
  client.socket.end();
});

test('a child ready after the side deadline keeps its lease for the next recall', async t => {
  const child = 'repo:example.test/child';
  const paths = await startService(t, paths => {
    registerChild(paths, child);
    return { env: { FAKE_NATIVE_RECALL_BY_PROJECT: JSON.stringify({ [`path:${paths.root}`]: projectRecall, 'user:owner': userRecall,
      [child]: { context: 'Child context', mode: 'tree', truncated: false, hits: [{ project: child, node: 'child-node' }] },
    }) }, beforeNative: `if (process.env.LAMBDA_MEMORY_PROJECT === ${JSON.stringify(child)}) {
      const fs = require('node:fs');
      fs.writeFileSync(${JSON.stringify(join(paths.root, 'child-pid'))}, String(process.pid));
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ${RECALL_SIDE_DEADLINE_MS + 400});
      fs.writeFileSync(${JSON.stringify(join(paths.root, 'child-started'))}, 'ready');
    }` };
  });
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const first = body(await call(client, 'memory_recall', { task: 'first' }));
  assert.equal(first.hits.some(hit => hit.scope === 'workspace'), false);
  await waitFor(() => existsSync(join(paths.root, 'child-started')));
  const pid = Number(readFileSync(join(paths.root, 'child-pid'), 'utf8'));
  await new Promise(resolve => setTimeout(resolve, 1200));
  assert.doesNotThrow(() => process.kill(pid, 0), 'the late-ready child lost its lease');
  const second = body(await call(client, 'memory_recall', { task: 'second' }));
  assert.equal(second.hits.at(-1).scope, 'workspace');
  assert.equal(calls(paths).find(entry => entry.project === child).pid, pid);
  client.socket.end();
});

test('workspace children join the recall', async t => {
  let childCwd;
  const childProject = 'repo:example.test/child';
  const paths = await startService(t, paths => {
    childCwd = join(paths.root, 'child'); mkdirSync(childCwd); mkdirSync(paths.data, { mode: 0o700 });
    writeFileSync(join(paths.data, 'projects.json'), JSON.stringify({ projects: [{ project: childProject, cwd: childCwd, last_seen_ms: 1 }] }), { mode: 0o600 });
    return { env: {
      FAKE_NATIVE_RECALL_BY_PROJECT: JSON.stringify({ [`path:${paths.root}`]: projectRecall, 'user:owner': userRecall,
        [childProject]: { context: 'Child context', mode: 'tree', truncated: false, hits: [{ project: childProject, node: 'child-node' }] } }),
      FAKE_NATIVE_BRIEF_BY_PROJECT: JSON.stringify({ [`path:${paths.root}`]: { roots: ['decisions'] }, 'user:owner': { roots: ['preferences'] }, [childProject]: { roots: ['architecture'] } }),
    } };
  });
  const client = connect(paths.socketPath, paths.root); await ready(client);
  const merged = body(await call(client, 'memory_recall', { task: 'question' }));
  assert.deepEqual(merged.hits.map(hit => [hit.node, hit.scope]), [['project-node', 'project'], ['user-node', 'user'], ['child-node', 'workspace']]);
  assert.match(merged.context, /Workspace memory \(child\):\nChild context$/);
  const first = calls(paths).find(entry => entry.project === childProject);
  await new Promise(resolve => setTimeout(resolve, 1100));
  const brief = body(await call(client, 'memory_brief', { project: `path:${paths.root}` }));
  assert.deepEqual(brief.workspace, [{ project: childProject, name: 'child', roots: ['architecture'] }]);
  assert.equal(calls(paths).filter(entry => entry.project === childProject).at(-1).pid, first.pid);
  client.socket.end();
});

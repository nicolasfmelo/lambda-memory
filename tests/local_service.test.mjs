import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { test } from 'node:test';
import { startupReason } from '../runtime/local-service.mjs';
import { VERSION } from '../runtime/version.mjs';

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

async function startService(t) {
  const paths = fixture();
  const service = spawn(process.execPath, [SERVICE], {
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

function connect(socketPath, cwd) {
  const socket = createConnection(socketPath);
  const lines = createInterface({ input: socket })[Symbol.asyncIterator]();
  let id = 0;
  const send = (message) => socket.write(`${JSON.stringify(message)}\n`);
  const next = async () => JSON.parse((await lines.next()).value);
  return {
    socket,
    async hello() { send({ cwd, token: TOKEN }); return next(); },
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

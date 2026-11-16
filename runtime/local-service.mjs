import { execFileSync } from 'node:child_process';
import { createHash, timingSafeEqual } from 'node:crypto';
import { chmodSync, lstatSync, mkdirSync, realpathSync, unlinkSync } from 'node:fs';
import { createConnection, createServer } from 'node:net';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadRegistry, recordProject } from './project-registry.mjs';
import { BRIEF_SIDE_DEADLINE_MS, CHILD_LEASE_MS, DRAIN_BACKOFF_MS, RECALL_SIDE_DEADLINE_MS,
  SERVICE_ONLY_TOOLS, SKILL_TOOLS, drainForwards, mergeBrief, mergeRecall, routeTool, workspaceChildren } from './cross-store.mjs';
export { BRIEF_SIDE_DEADLINE_MS, CHILD_LEASE_MS, DRAIN_BACKOFF_MS, RECALL_SIDE_DEADLINE_MS } from './cross-store.mjs';
import { VERSION } from './version.mjs';
import { NativeMcpClient, PROTOCOL_VERSION } from '../extensions/lambda-memory.mjs';
import { loadConfig, resolveProjectBindingAsync, safeEnvironment, nativeEnvironment, userIdentity } from './project-binding.mjs';
import { frames, writeFrame, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES } from './framing.mjs';
import { daemonWebStarter } from './web-service.mjs';

function privateDirectory(path) {
  mkdirSync(path, { recursive: true, mode: 0o700 });
  const stat = lstatSync(path);
  if (!stat.isDirectory() || stat.uid !== process.getuid() || (stat.mode & 0o077)) throw new Error('private directory required');
}
function authorized(value, token) {
  if (typeof value !== 'string') return false;
  const left = Buffer.from(value), right = Buffer.from(token);
  return left.length === right.length && timingSafeEqual(left, right);
}
const error = (id, code, message) => ({ jsonrpc: '2.0', id, error: { code, message } });
const success = (id, result) => ({ jsonrpc: '2.0', id, result });
function validRequest(request) {
  if (!request || typeof request !== 'object' || Array.isArray(request) || request.jsonrpc !== '2.0' || typeof request.method !== 'string') return false;
  if (Object.keys(request).some(key => !['jsonrpc', 'id', 'method', 'params'].includes(key))) return false;
  return !Object.hasOwn(request, 'id') || request.id === null || Number.isSafeInteger(request.id) || (typeof request.id === 'string' && Buffer.byteLength(request.id) <= 512);
}

async function prepareSocket(path) {
  let previous;
  try { previous = lstatSync(path); } catch (failure) { if (failure.code === 'ENOENT') return; throw failure; }
  if (!previous.isSocket() || previous.uid !== process.getuid()) throw new Error('unexpected socket path');
  const active = await new Promise((resolve, reject) => {
    const probe = createConnection(path);
    probe.once('connect', () => { probe.destroy(); resolve(true); });
    probe.once('error', failure => { probe.destroy(); if (failure.code === 'ECONNREFUSED') resolve(false); else reject(new Error('socket unavailable')); });
    probe.setTimeout(1000, () => { probe.destroy(); reject(new Error('socket probe timeout')); });
  });
  if (active) throw new Error('service already running');
  const current = lstatSync(path);
  if (current.ino !== previous.ino || current.dev !== previous.dev) throw new Error('socket changed');
  unlinkSync(path);
}

/**
 * Runs the daemon until SIGTERM/SIGINT. `startWeb`, when given, is called once after the socket is
 * listening with `{ config, acquire, release, workers, registry }` and may return `{ stop() }`
 * (sync or async), which is awaited during shutdown. The registry object is
 * `{ record(binding), recordProject }` over `config.dataDir`.
 */
export async function serve(config, { startWeb, discoverChildren = workspaceChildren } = {}) {
  privateDirectory(config.dataDir); privateDirectory(dirname(config.socketPath));
  const providerEnvironment = nativeEnvironment(config);
  await prepareSocket(config.socketPath);
  const workers = new Map(), connections = new Set(), sockets = new Set();
  let stopping = false, ownedSocket;
  const userBinding = { project: userIdentity(config.owner), cwd: config.dataDir };
  const childLeases = new Map(), childDiscoveries = new Map(), drainBackoff = new Map();
  let userLease, userLeasePending;
  const readyProjects = () => [...workers.values()].some(worker => worker.project !== userBinding.project && worker.live && !worker.closing);
  const releaseLater = worker => { release(worker).catch(() => {}); };
  function ensureUserLease() {
    if (stopping || userLease || userLeasePending || !readyProjects()) return;
    // Never put user startup on a project's acquire/hello critical path.
    userLeasePending = acquire(userBinding).then(worker => {
      userLeasePending = null;
      if (stopping || !readyProjects()) releaseLater(worker);
      else userLease = worker;
    }, () => { userLeasePending = null; });
  }
  function dropUserLease() {
    if (userLease && (stopping || !readyProjects())) {
      const worker = userLease; userLease = null; releaseLater(worker);
    }
  }
  async function invoke(worker, name, args, options = {}) {
    if (stopping || worker.closing) throw new Error('service stopping');
    await worker.client.ensureReady();
    if (stopping || worker.closing) throw new Error('service stopping');
    return worker.client.callTool(name, args, { mutating: worker.client.tools.get(name)?.annotations?.readOnlyHint !== true, ...options });
  }
  function startDrain(worker) {
    if (worker.drainTimer || !worker.client.tools.has('memory_forwards')) return;
    const drain = async () => {
      if (stopping || worker.closing || worker.draining || Date.now() < (drainBackoff.get(worker.project) || 0)) return;
      drainBackoff.delete(worker.project);
      worker.draining = true;
      ensureUserLease();
      let userWorker;
      try {
        const result = await drainForwards({ project: worker.project, user: userBinding.project,
          stopped: () => stopping || Boolean(worker.closing),
          callProject: (name, args) => invoke(worker, name, args),
          callUser: async (name, args) => {
            if (!userWorker) userWorker = await acquire(userBinding);
            return invoke(userWorker, name, args);
          } });
        if (result.backoff) drainBackoff.set(worker.project, Date.now() + DRAIN_BACKOFF_MS);
      } catch { /* forwards remain durable and pending */ }
      finally { worker.draining = false; if (userWorker) releaseLater(userWorker); }
    };
    worker.drainTimer = setInterval(() => { drain().catch(() => {}); }, 60000);
    worker.drainTimer.unref();
    drain().catch(() => {});
  }
  async function acquire(binding) {
    for (;;) {
      if (stopping) throw new Error('service stopping');
      let worker = workers.get(binding.project);
      if (worker?.closing) { await worker.closing; continue; }
      if (!worker) {
        const store = join(config.dataDir, createHash('sha256').update(binding.project).digest('hex'), 'store');
        privateDirectory(dirname(store));
        const env = { ...safeEnvironment(), ...providerEnvironment, LAMBDA_MEMORY_DATA_DIR: store, LAMBDA_MEMORY_OWNER: config.owner, LAMBDA_MEMORY_ACTOR: config.actor, LAMBDA_MEMORY_PROJECT: binding.project, LAMBDA_MEMORY_TOKEN: config.token };
        const client = new NativeMcpClient({ bin: config.nativeBin, env, cwd: binding.cwd, timeoutMs: config.mode === 'openrouter' ? 600000 : 30000, shutdownMs: config.mode === 'openrouter' ? 630000 : 1000 });
        worker = { project: binding.project, client, users: 0, ready: client.ensureReady(), closing: null };
        workers.set(binding.project, worker);
      }
      worker.users += 1;
      try {
        await worker.ready;
        worker.live = true;
        if (worker.project !== userBinding.project) { ensureUserLease(); startDrain(worker); }
        return worker;
      } catch (failure) { await release(worker); throw failure; }
    }
  }
  async function release(worker) {
    worker.users -= 1;
    if (worker.users || worker.closing) return;
    worker.live = false;
    clearInterval(worker.drainTimer);
    worker.closing = (async () => {
      try { await worker.ready; } catch { /* failed startup still needs disposal */ }
      await worker.client.dispose();
      if (workers.get(worker.project) === worker) workers.delete(worker.project);
    })();
    dropUserLease();
    await worker.closing;
  }
  async function holdChild(binding) {
    let lease = childLeases.get(binding.project);
    if (!lease) {
      lease = { worker: null, timer: null, pending: null };
      childLeases.set(binding.project, lease);
      lease.pending = acquire(binding).then(worker => {
        lease.worker = worker;
        if (stopping || childLeases.get(binding.project) !== lease) { releaseLater(worker); lease.worker = null; }
      }).catch(failure => { if (childLeases.get(binding.project) === lease) childLeases.delete(binding.project); throw failure; });
    }
    await lease.pending;
    if (stopping) return;
    clearTimeout(lease.timer);
    lease.timer = setTimeout(() => {
      if (childLeases.get(binding.project) !== lease) return;
      childLeases.delete(binding.project);
      if (lease.worker) releaseLater(lease.worker);
    }, CHILD_LEASE_MS);
    lease.timer.unref();
  }
  async function byDeadline(work, expires, fallback) {
    let timer;
    try {
      return await Promise.race([work, new Promise(resolve => {
        timer = setTimeout(() => resolve(fallback), Math.max(0, expires - Date.now())); timer.unref();
      })]);
    } finally { clearTimeout(timer); }
  }
  function cachedChildren(binding) {
    // Children depend on the checkout directory, not only on the identity (worktrees share remotes).
    const key = `${binding.project}\0${binding.cwd}`;
    let cached = childDiscoveries.get(key);
    if (!cached || Date.now() >= cached.until) {
      // Cache in-flight discovery too; a slow registry/path lookup must not multiply per recall.
      const work = Promise.resolve().then(() => {
        let entries = [];
        try { entries = loadRegistry(config.dataDir); } catch { /* unavailable registry has no children */ }
        return discoverChildren(binding, userBinding.project, entries, config.ignoreRoots);
      }).catch(() => []);
      cached = { until: Date.now() + 60000, work };
      childDiscoveries.set(key, cached);
    }
    return cached.work;
  }
  async function sideCall(binding, name, args, expires, child = false) {
    ensureUserLease();
    const work = (async () => {
      const worker = await acquire(binding);
      let heldChild = false;
      try {
        // A cold child can outlive this reply's deadline; retain it for the next recall anyway.
        if (child && !stopping) { await holdChild(binding); heldChild = true; }
        if (Date.now() >= expires || stopping) return null;
        return await invoke(worker, name, args, { canAdmit: () => !stopping && Date.now() < expires });
      } finally {
        if (heldChild && !stopping) holdChild(binding).catch(() => {});
        releaseLater(worker);
      }
    })().catch(() => null);
    return byDeadline(work, expires, null);
  }
  async function callRouted(worker, binding, name, args, route) {
    if (route.target === 'project') return invoke(worker, name, args);
    ensureUserLease();
    if (route.target === 'user') {
      const user = await acquire(userBinding);
      try { return await invoke(user, name, args); } finally { releaseLater(user); }
    }
    const expires = Date.now() + (name === 'memory_recall' ? RECALL_SIDE_DEADLINE_MS : BRIEF_SIDE_DEADLINE_MS);
    // Discovery, user startup and the primary run in parallel under one shared side deadline.
    const primary = invoke(worker, name, args);
    const sideArgs = project => name === 'memory_recall' ? { ...args, projects: [project] } : { ...args, project };
    const user = sideCall(userBinding, name, sideArgs(userBinding.project), expires);
    const workspaceWork = byDeadline(cachedChildren(binding), expires, []).then(children => {
      if (stopping || Date.now() >= expires) return [];
      return Promise.all(children.map(async child => ({ ...child,
        result: await sideCall(child, name, sideArgs(child.project), expires, true) })));
    });
    const [projectResult, userResult, workspace] = await Promise.all([primary, user, workspaceWork]);
    return name === 'memory_recall' ? mergeRecall(projectResult, userResult, workspace, args.budget_bytes)
      : mergeBrief(projectResult, userResult, workspace);
  }
  async function connection(socket) {
    let worker, phase = 'new';
    try {
      const input = frames(socket, MAX_REQUEST_BYTES);
      const first = await input.next();
      const hello = JSON.parse(first.value);
      if (!hello || typeof hello !== 'object' || Array.isArray(hello)
          || Object.keys(hello).some(key => !['cwd', 'token', 'operator', 'store'].includes(key))
          || (Object.hasOwn(hello, 'operator') && hello.operator !== true)
          || (Object.hasOwn(hello, 'store') && (hello.store !== 'user' || hello.operator !== true))
          || !authorized(hello.token, config.token)) throw new Error('connection rejected');
      const operator = hello.operator === true;
      const binding = hello.store === 'user' ? userBinding : await resolveProjectBindingAsync(hello.cwd);
      if (stopping) throw new Error('service stopping');
      worker = await acquire(binding);
      if (hello.store !== 'user') recordProject(config.dataDir, { project: binding.project, cwd: binding.cwd }).catch(() => {});
      await writeFrame(socket, { ready: true, ...binding }, MAX_RESPONSE_BYTES);
      for await (const text of input) {
        let request;
        try { request = JSON.parse(text); } catch { await writeFrame(socket, error(null, -32700, 'parse error'), MAX_RESPONSE_BYTES); continue; }
        if (!validRequest(request)) { await writeFrame(socket, error(null, -32600, 'invalid request'), MAX_RESPONSE_BYTES); continue; }
        if (!Object.hasOwn(request, 'id')) {
          if (request.method === 'notifications/initialized' && phase === 'initialized') phase = 'ready';
          continue;
        }
        const { id, method } = request; const params = request.params;
        let reply;
        if (method === 'initialize') {
          if (phase !== 'new') reply = error(id, -32600, 'already initialized');
          else if (!authorized(params?._meta?.lambdaMemoryToken, config.token)) { reply = error(id, -32001, 'unauthorized'); phase = 'denied'; }
          else if (!params || typeof params.protocolVersion !== 'string' || !params.capabilities || typeof params.capabilities !== 'object' || Array.isArray(params.capabilities) || !params.clientInfo || typeof params.clientInfo !== 'object' || Array.isArray(params.clientInfo)) reply = error(id, -32602, 'invalid initialization parameters');
          else {
            phase = 'initialized';
            const version = ['2025-11-25', '2025-06-18', '2024-11-05'].includes(params.protocolVersion) ? params.protocolVersion : PROTOCOL_VERSION;
            reply = success(id, { protocolVersion: version, capabilities: { tools: {} }, serverInfo: { name: 'lambda-memory', version: VERSION } });
          }
        } else if (method === 'notifications/initialized') reply = error(id, -32600, 'initialized must be a notification');
        else if (method === 'ping' && phase !== 'new') reply = success(id, {});
        else if (['tools/list', 'tools/call', 'ping'].includes(method) && phase !== 'ready') reply = error(id, -32600, 'session not ready');
        else if (method === 'tools/list') {
          // A crashed native child clears its tools; restart it instead of listing nothing.
          try {
            await worker.client.ensureReady();
            const tools = [...worker.client.tools.values()].filter(tool => !SERVICE_ONLY_TOOLS.has(tool.name) || (operator && SKILL_TOOLS.has(tool.name))).map(tool => tool.name !== 'memory_capture' ? tool : {
              ...tool, inputSchema: { ...tool.inputSchema, properties: { ...tool.inputSchema?.properties, scope: { type: 'string', enum: ['project', 'user'] } } },
            });
            reply = success(id, { tools });
          }
          catch { reply = error(id, -32603, 'memory service unavailable'); }
        }
        else if (method === 'tools/call') {
          if (stopping) reply = error(id, -32603, 'service stopping');
          else if (!params || typeof params !== 'object' || Array.isArray(params) || Object.keys(params).some(key => !['name', 'arguments', '_meta'].includes(key)) || typeof params.name !== 'string' || (params.arguments !== undefined && (!params.arguments || typeof params.arguments !== 'object' || Array.isArray(params.arguments)))) reply = error(id, -32602, 'invalid params');
          else if (SERVICE_ONLY_TOOLS.has(params.name) && !(operator && SKILL_TOOLS.has(params.name))) reply = error(id, -32602, 'unknown tool');
          else {
            try {
              const route = routeTool(params.name, params.arguments || {}, binding.project, userBinding.project, { operator });
              if (!(await worker.client.ensureReady().then(() => worker.client.tools.has(params.name), () => true))) reply = error(id, -32602, 'unknown tool');
              else reply = success(id, await callRouted(worker, binding, params.name, route.args, route));
            }
            catch (failure) {
              // Requests refused locally never reached the native process and are safe to retry.
              reply = failure?.rpcCode === -32602 && ['unknown tool', 'invalid params'].includes(failure.message)
                ? error(id, -32602, failure.message)
                : failure?.message === 'pending bound reached'
                ? error(id, -32603, 'memory service busy; not sent, safe to retry')
                : failure?.rpcCode === -32602
                  ? error(id, -32602, 'invalid tool arguments; nothing was written. Check every required field against the tool input schema (tools/list)')
                  : error(id, -32603, 'operation not confirmed; do not automatically retry writes');
            }
          }
        } else reply = error(id, -32601, 'method not found');
        await writeFrame(socket, reply, MAX_RESPONSE_BYTES);
        if (phase === 'denied') break;
      }
    } catch { /* no request text, identities or secret diagnostics */ }
    finally {
      // Close the client side first: releasing the last user may wait for native shutdown.
      sockets.delete(socket); socket.end();
      if (worker) await release(worker);
    }
  }
  const server = createServer({ allowHalfOpen: true }, socket => {
    if (stopping) { socket.destroy(); return; }
    sockets.add(socket); socket.on('error', () => {});
    const work = connection(socket);
    connections.add(work); work.finally(() => connections.delete(work)).catch(() => socket.destroy());
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(config.socketPath, () => { server.off('error', reject); resolve(); }); });
  chmodSync(config.socketPath, 0o600); ownedSocket = lstatSync(config.socketPath);
  let web;
  const stop = () => {
    stopping = true; server.close(); for (const socket of sockets) socket.destroy();
    for (const worker of workers.values()) clearInterval(worker.drainTimer);
    for (const lease of childLeases.values()) { clearTimeout(lease.timer); if (lease.worker) releaseLater(lease.worker); }
    childLeases.clear(); dropUserLease();
  };
  try {
    if (startWeb) web = await startWeb({ config, acquire, release, workers, registry: { record: binding => recordProject(config.dataDir, { project: binding.project, cwd: binding.cwd }), recordProject: (entry, nowMs) => recordProject(config.dataDir, entry, nowMs) } });
    if (process.env.NOTIFY_SOCKET) {
      execFileSync('/usr/bin/systemd-notify', ['--ready'], {
        env: { ...safeEnvironment(), NOTIFY_SOCKET: process.env.NOTIFY_SOCKET },
        stdio: 'ignore',
        timeout: 10000,
      });
    }
    console.log('Lambda Memory local service ready');
    await new Promise((resolve, reject) => {
      server.once('close', resolve); server.on('error', reject);
      process.once('SIGTERM', stop); process.once('SIGINT', stop);
    });
  } finally {
    stop(); await Promise.allSettled([web?.stop?.(), ...connections]);
    await Promise.allSettled([...workers.values()].map(async worker => { try { await worker.ready; } catch {} await worker.client.dispose(); }));
    process.off('SIGTERM', stop); process.off('SIGINT', stop);
    try { const stat = lstatSync(config.socketPath); if (stat.ino === ownedSocket.ino && stat.dev === ownedSocket.dev) unlinkSync(config.socketPath); } catch (failure) { if (failure.code !== 'ENOENT') throw failure; }
  }
}

/** A safe, actionable startup message. Never echoes file contents (JSON errors can quote the token). */
export function startupReason(failure) {
  if (failure instanceof SyntaxError) return 'configuration is not valid JSON';
  if (failure?.code === 'ENOENT') return `missing file: ${failure.path ?? 'configuration'}`;
  if (failure?.code === 'EACCES') return `permission denied: ${failure.path ?? 'configuration'}`;
  const known = {
    'private configuration required': 'configuration must be a regular file owned by you with mode 0600 (chmod 600)',
    'private provider environment required': 'providerEnvFile must be an absolute path to a file owned by you with mode 0600',
    'socket path bound': 'socketPath is longer than 100 bytes; use a shorter path such as $XDG_RUNTIME_DIR/lambda-memory/service.sock',
    'invalid token': 'token must be 32-1024 printable ASCII characters without spaces (openssl rand -hex 32)',
    'provider key required': 'OPEN_ROUTER_KEY is missing from the provider environment file',
    'unreviewed runtime model profile': 'model variables must match the pinned models (see .env.example)',
  };
  const message = typeof failure?.message === 'string' ? failure.message : 'unknown error';
  return known[message] ?? `${message}; check the configuration and store directory ownership`;
}

const entry = process.argv[1] ? (() => { try { return realpathSync(process.argv[1]); } catch { return ''; } })() : '';
if (entry === realpathSync(fileURLToPath(import.meta.url))) {
  try { const config = loadConfig(); await serve(config, { startWeb: daemonWebStarter(config) }); }
  catch (failure) { console.error(`Lambda Memory local service failed: ${startupReason(failure)}`); process.exitCode = 1; }
}

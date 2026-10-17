import { randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadRegistry } from './project-registry.mjs';
import { resolveProjectBindingAsync } from './project-binding.mjs';

export const MAX_BODY_BYTES = 64 * 1024;
export const MAX_UI_LEASES = 4;
export const CSRF_HEADER = 'x-lambda-memory-csrf';
export const CSRF_PLACEHOLDER = '__LM_CSRF__';
// memory_purge is reachable only through /api/purge, which requires an explicit confirmation.
export const UI_TOOLS = new Set(['memory_status', 'memory_activity', 'memory_list', 'memory_timeline', 'memory_timeline_write', 'memory_review']);
const WEB_DIR = join(dirname(fileURLToPath(import.meta.url)), 'web');
const STATIC = {
  '/': ['index.html', 'text/html; charset=utf-8'], '/app.js': ['app.js', 'text/javascript; charset=utf-8'], '/app.css': ['app.css', 'text/css; charset=utf-8'],
  '/fonts/fira-code-latin.woff2': ['fonts/fira-code-latin.woff2', 'font/woff2'],
  '/fonts/fira-code-latin-ext.woff2': ['fonts/fira-code-latin-ext.woff2', 'font/woff2'],
  '/fonts/fira-code-greek.woff2': ['fonts/fira-code-greek.woff2', 'font/woff2'],
};
const SECURITY_HEADERS = {
  'Content-Security-Policy': "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
  'X-Content-Type-Options': 'nosniff',
  'Referrer-Policy': 'no-referrer',
  'Cache-Control': 'no-store',
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Resource-Policy': 'same-origin',
};
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const NOT_CONFIRMED = { retry: false, message: 'operation not confirmed; do not automatically retry writes' };

class HttpError extends Error {
  constructor(status, body) { super(body.error ?? 'request failed'); this.status = status; this.body = body; }
}
const fail = (status, error) => new HttpError(status, { error });

export function allowedHost(host, port) {
  return typeof host === 'string' && (host === `127.0.0.1:${port}` || host === `localhost:${port}`);
}
export function allowedOrigin(origin, port) {
  return typeof origin === 'string' && (origin === `http://127.0.0.1:${port}` || origin === `http://localhost:${port}`);
}

/**
 * The uid owning the client's end of an ESTABLISHED loopback connection: local=127.0.0.1:peer,
 * remote=127.0.0.1:server. `0100007F` is 127.0.0.1 as little-endian hex; big-endian hosts never
 * match and fail closed. Stale rows (TIME_WAIT etc.) are ignored.
 */
export function parseProcNetTcp(text, peerPort, serverPort) {
  const hex = port => port.toString(16).toUpperCase().padStart(4, '0');
  const local = `0100007F:${hex(peerPort)}`, remote = `0100007F:${hex(serverPort)}`;
  for (const line of String(text).split('\n').slice(1)) {
    const fields = line.trim().split(/\s+/);
    if (fields.length < 8 || fields[1] !== local || fields[2] !== remote || fields[3] !== '01' || !/^\d+$/.test(fields[7])) continue;
    return Number(fields[7]);
  }
  return null;
}
const defaultProcReader = () => readFileSync('/proc/net/tcp', 'utf8');

/** Reads a JSON body up to `cap` bytes; rejects with an HttpError (413 oversize, 400 invalid). */
export function readJsonBody(req, cap = MAX_BODY_BYTES) {
  return new Promise((resolve, reject) => {
    const declared = Number(req.headers['content-length']);
    if (Number.isFinite(declared) && declared > cap) { reject(fail(413, 'payload too large')); return; }
    const chunks = []; let size = 0, done = false;
    const finish = (failure, value) => { if (done) return; done = true; if (failure) reject(failure); else resolve(value); };
    req.on('data', chunk => {
      if (done) return;
      size += chunk.length;
      if (size > cap) { finish(fail(413, 'payload too large')); req.pause(); return; }
      chunks.push(chunk);
    });
    req.on('error', () => finish(fail(400, 'invalid body')));
    req.on('end', () => {
      try { finish(null, JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)))); }
      catch { finish(fail(400, 'invalid body')); }
    });
  });
}

function authorized(value, token) {
  if (typeof value !== 'string') return false;
  const left = Buffer.from(value), right = Buffer.from(token);
  return left.length === right.length && timingSafeEqual(left, right);
}
const plainObject = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const text = value => typeof value === 'string' && value.length > 0 && Buffer.byteLength(value) <= MAX_BODY_BYTES;
// Mirrors the native identity rules: non-blank, at most 512 UTF-8 bytes, no control characters.
const identity = value => typeof value === 'string' && value.trim().length > 0 && Buffer.byteLength(value) <= 512 && !/\p{Cc}/u.test(value);
function shape(body, required, optional = []) {
  if (!plainObject(body) || Object.keys(body).some(key => !required.includes(key) && !optional.includes(key)) || required.some(key => !Object.hasOwn(body, key))) throw fail(400, 'invalid body');
  if (!text(body.project) || Buffer.byteLength(body.project) > 512) throw fail(400, 'invalid body');
}
function toolResult(result) {
  const joined = (Array.isArray(result?.content) ? result.content : []).filter(part => part?.type === 'text' && typeof part.text === 'string').map(part => part.text).join('');
  let body;
  try { body = JSON.parse(joined); } catch { body = joined; }
  return { isError: result?.isError === true, body };
}

/**
 * Serves the local web UI on 127.0.0.1:`config.webPort`. Curation calls go through the daemon's
 * native workers via `acquire`/`release`; each project has at most one UI lease renewed per call.
 * Returns `{ server, close() }`; refuses to start when the peer-uid source is unreadable.
 */
export async function startWebService({ config, acquire, release, workers, resolveBinding = resolveProjectBindingAsync, readProcNetTcp = defaultProcReader, webDir = WEB_DIR }) {
  const port = config.webPort;
  readProcNetTcp({ peerPort: 0, serverPort: port }); // fail closed without /proc
  const csrf = randomBytes(32).toString('hex');
  const files = new Map();
  for (const [path, [name, type]] of Object.entries(STATIC)) {
    let body = readFileSync(join(webDir, name));
    if (name === 'index.html') {
      const content = body.toString('utf8');
      if (!content.includes(CSRF_PLACEHOLDER)) throw new Error('index placeholder missing');
      body = Buffer.from(content.replace(CSRF_PLACEHOLDER, csrf));
    }
    files.set(path, { body, type });
  }
  const uiLeases = new Map(); // project -> { worker, timer, pending }
  const peers = new WeakMap(); // socket -> boolean
  const inFlight = new Set(); // request handler promises; shutdown waits for them
  let closed = false;

  function peerAllowed(socket) {
    if (peers.has(socket)) return peers.get(socket);
    let allowed = false;
    try {
      allowed = socket.remoteAddress === '127.0.0.1' && socket.localPort === port
        && parseProcNetTcp(readProcNetTcp({ peerPort: socket.remotePort, serverPort: port }), socket.remotePort, port) === process.getuid();
    } catch { allowed = false; }
    peers.set(socket, allowed);
    return allowed;
  }
  const live = project => uiLeases.has(project) || (workers.has(project) && !workers.get(project).closing);

  function expire(project, lease) {
    if (uiLeases.get(project) !== lease) return;
    uiLeases.delete(project); clearTimeout(lease.timer);
    if (lease.worker) release(lease.worker).catch(() => {});
  }
  function renew(project, lease) {
    clearTimeout(lease.timer);
    lease.timer = setTimeout(() => expire(project, lease), config.webLeaseMs);
    lease.timer.unref?.();
  }
  async function holdLease(binding) {
    let lease = uiLeases.get(binding.project);
    if (!lease) {
      if (closed) throw fail(503, 'service stopping');
      if (uiLeases.size >= MAX_UI_LEASES) throw fail(429, 'too many active projects');
      lease = { worker: null, timer: null, pending: null };
      uiLeases.set(binding.project, lease);
      lease.pending = acquire(binding).then(worker => {
        lease.worker = worker;
        if (closed || uiLeases.get(binding.project) !== lease) {
          if (uiLeases.get(binding.project) === lease) uiLeases.delete(binding.project);
          release(worker).catch(() => {}); lease.worker = null; throw fail(503, 'service stopping');
        }
      }, failure => { if (uiLeases.get(binding.project) === lease) uiLeases.delete(binding.project); throw failure; });
    }
    await lease.pending;
    renew(binding.project, lease);
  }
  async function bound(project) {
    let entries;
    try { entries = loadRegistry(config.dataDir); } catch { throw fail(500, 'project registry unavailable'); }
    const entry = entries.find(item => item.project === project);
    if (!entry) throw fail(404, 'unknown project');
    let binding;
    try { binding = await resolveBinding(entry.cwd); } catch { throw fail(409, 'project binding changed'); }
    if (binding.project !== project) throw fail(409, 'project binding changed');
    return binding;
  }
  async function guarded(work) {
    try { return await work(); }
    catch (failure) {
      if (failure instanceof HttpError) throw failure;
      // -32602: the native server rejected the arguments before running the tool; nothing was written.
      if (failure?.rpcCode === -32602) throw fail(400, 'invalid arguments');
      if (failure?.message === 'pending bound reached') throw new HttpError(503, { retry: true, message: 'memory service busy; not sent, safe to retry' });
      throw new HttpError(502, NOT_CONFIRMED);
    }
  }
  // Runs `work(callTool)` under the UI lease and an in-flight reference of its own.
  async function withProject(project, work, { start = false } = {}) {
    const binding = await bound(project);
    if (closed) throw fail(503, 'service stopping');
    if (!start && !live(project)) throw new HttpError(409, { needs_start: true });
    return guarded(async () => {
      await holdLease(binding);
      if (closed) throw fail(503, 'service stopping'); // no new worker reference once close begins
      const worker = await acquire(binding);
      try {
        const invoke = async (name, args) => {
          await worker.client.ensureReady();
          return toolResult(await worker.client.callTool(name, args, { mutating: worker.client.tools.get(name)?.annotations?.readOnlyHint !== true }));
        };
        return await work(invoke);
      } finally {
        await release(worker).catch(() => {});
        const lease = uiLeases.get(project);
        if (lease?.worker) renew(project, lease);
      }
    });
  }

  const routes = {
    'GET /api/projects': async () => {
      let entries;
      try { entries = loadRegistry(config.dataDir); } catch { throw fail(500, 'project registry unavailable'); }
      return { projects: entries.map(({ project, cwd, last_seen_ms }) => ({ project, cwd, last_seen_ms, live: live(project) })) };
    },
    'POST /api/start': async body => {
      shape(body, ['project']);
      return withProject(body.project, async () => ({ started: true, project: body.project }), { start: true });
    },
    'POST /api/tool': async body => {
      shape(body, ['project', 'name'], ['arguments']);
      const args = body.arguments ?? {};
      if (typeof body.name !== 'string' || !UI_TOOLS.has(body.name) || !plainObject(args)) throw fail(400, 'invalid body');
      if (Object.hasOwn(args, 'owner') || Object.hasOwn(args, 'actor')) throw fail(400, 'identity is fixed by the daemon');
      if (Object.hasOwn(args, 'project') && args.project !== body.project) throw fail(400, 'project mismatch');
      return withProject(body.project, invoke => invoke(body.name, { ...args, project: body.project }));
    },
    'POST /api/purge': async body => {
      shape(body, ['project', 'sources', 'confirm']);
      if (!Array.isArray(body.sources) || body.sources.length !== 1 || !text(body.sources[0]) || body.confirm !== body.sources[0]) throw fail(400, 'confirmation must equal the single source');
      return withProject(body.project, invoke => invoke('memory_purge', { project: body.project, sources: [body.sources[0]] }));
    },
    'POST /api/correct': async body => {
      shape(body, ['project', 'memory', 'predecessor', 'content'], ['nonce']);
      if (!identity(body.memory) || !identity(body.predecessor) || !text(body.content)) throw fail(400, 'invalid body');
      if (body.nonce !== undefined && (typeof body.nonce !== 'string' || !UUID_V4.test(body.nonce))) throw fail(400, 'invalid body');
      return withProject(body.project, async invoke => {
        // A client nonce makes a retry replay the same source and revision instead of duplicating them.
        const id = body.nonce ?? randomUUID(), source = `ui/${id}`;
        const capture = await invoke('memory_capture', { project: body.project, source, kind: 'decision', outcome: 'unknown', origin: 'ui://manual-curation', content: body.content });
        if (capture.isError) return { capture };
        let propose;
        try {
          propose = await invoke('memory_propose', { project: body.project, memory: body.memory, revision: `ui-${id}`, sources: [source], content: body.content, predecessor: body.predecessor });
        } catch {
          // The evidence is stored; report it so the UI does not claim nothing was written.
          throw new HttpError(502, { ...NOT_CONFIRMED, capture_saved: true, capture });
        }
        return { capture, propose };
      });
    },
  };

  function send(res, status, body, type = 'application/json; charset=utf-8', extra = {}) {
    const payload = Buffer.isBuffer(body) ? body : Buffer.from(JSON.stringify(body));
    res.writeHead(status, { ...SECURITY_HEADERS, 'Content-Type': type, 'Content-Length': payload.length, ...extra });
    res.end(payload);
  }
  async function handle(req, res) {
    const path = String(req.url).split('?')[0];
    const { host, origin } = req.headers, site = req.headers['sec-fetch-site'];
    if (!peerAllowed(req.socket) || !allowedHost(host, port)) throw fail(403, 'forbidden');
    if (origin !== undefined && (!allowedOrigin(origin, port) || origin !== `http://${host}`)) throw fail(403, 'forbidden');
    if (site !== undefined && site !== 'same-origin' && site !== 'none') throw fail(403, 'forbidden');
    if (Object.hasOwn(STATIC, path)) {
      if (req.method !== 'GET') throw fail(405, 'method not allowed');
      const file = files.get(path);
      return send(res, 200, file.body, file.type);
    }
    if (!path.startsWith('/api/')) throw fail(404, 'not found');
    if (req.method === 'OPTIONS') throw fail(405, 'method not allowed'); // no CORS preflight
    if (!authorized(req.headers[CSRF_HEADER], csrf)) throw fail(403, 'forbidden');
    const route = routes[`${req.method} ${path}`];
    if (!route) throw fail(Object.keys(routes).some(key => key.endsWith(` ${path}`)) ? 405 : 404, 'not found');
    if (closed) throw fail(503, 'service stopping');
    let body;
    if (req.method === 'POST') {
      if (origin === undefined) throw fail(403, 'forbidden');
      if (!/^application\/json\s*(;|$)/i.test(String(req.headers['content-type'] ?? ''))) throw fail(415, 'application/json required');
      body = await readJsonBody(req, MAX_BODY_BYTES);
    }
    return send(res, 200, await route(body));
  }

  const server = createServer({ requestTimeout: 30000, headersTimeout: 10000, keepAliveTimeout: 5000, maxHeaderSize: 16384 }, (req, res) => {
    const work = handle(req, res).catch(failure => {
      const status = failure instanceof HttpError ? failure.status : 500;
      const body = failure instanceof HttpError ? failure.body : { error: 'request failed' };
      if (res.headersSent) { res.destroy(); return; }
      // Unread request bodies (oversize, refused) must not keep the connection alive.
      send(res, status, body, undefined, req.complete ? {} : { Connection: 'close' });
    });
    inFlight.add(work); work.finally(() => inFlight.delete(work)).catch(() => {});
  });
  server.on('clientError', (_failure, socket) => socket.destroy());
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen({ host: '127.0.0.1', port, exclusive: true }, () => { server.off('error', reject); resolve(); });
  });

  async function close() {
    if (closed) return;
    closed = true;
    // Stop accepting, let in-flight calls settle (and answer), then drop every connection.
    const stopped = new Promise(resolve => server.close(() => resolve()));
    server.closeIdleConnections();
    while (inFlight.size) await Promise.allSettled([...inFlight]);
    server.closeAllConnections();
    await stopped;
    const leases = [...uiLeases.values()]; uiLeases.clear();
    await Promise.allSettled(leases.map(async lease => {
      clearTimeout(lease.timer);
      try { await lease.pending; } catch { return; }
      if (lease.worker) await release(lease.worker);
    }));
  }
  return { server, close };
}

/** The daemon's `startWeb` hook: undefined without `webPort`; startup failures keep the socket service running. */
export function daemonWebStarter(config, options = {}) {
  if (!config.webPort) return undefined;
  return async context => {
    try {
      const web = await startWebService({ ...context, ...options });
      console.log(`Lambda Memory web UI listening on http://127.0.0.1:${context.config.webPort}`);
      return { stop: () => web.close() };
    } catch {
      console.error('Lambda Memory web UI unavailable; the local socket service keeps running.');
      return { stop() {} };
    }
  };
}

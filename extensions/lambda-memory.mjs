import { spawn } from 'node:child_process';
import { homedir } from 'node:os';
import { resolve } from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { randomUUID } from 'node:crypto';
import { buildEpisode, READ_ONLY_TOOLS } from '../runtime/action-episode.mjs';
import { describeOmp } from '../runtime/hook-adapters/tool-kinds.mjs';
import { VERSION } from '../runtime/version.mjs';

export const RECALL_CUSTOM_TYPE = 'lambda-memory-recall';
export const PLAYBOOK_CUSTOM_TYPE = 'lambda-memory-playbook';
export const PROTOCOL_VERSION = '2025-11-25';
export const RECALL_TOOL = 'memory_recall';
export const CAPTURE_TOOL = 'memory_capture';
export const PLAYBOOK_TOOL = 'memory_playbook';
const DEFAULT_BIN = 'lambda-memory';
const SERVE_ARGV = ['--serve-mcp'];
const REQUEST_TIMEOUT_MS = 2000;
const PLAYBOOK_MS = 2000;
const AUTOMATIC_RECALL_MS = 25000;
const DISPOSE_DRAIN_MS = 500;
const CHILD_EXIT_MS = 1000;
const MAX_RESPONSE_BYTES = 1024 * 1024;
const MAX_PENDING = 8;
const CAPTURE_QUEUE_MAX = 32;
const MAX_TASK_BYTES = 4_000;
const MAX_ID_CHARS = 128;
const MAX_TOOL_NAME_CHARS = 128;
const MIN_TOKEN_CHARS = 32;
const MAX_TOKEN_CHARS = 1024;
const CLIENT_INFO = { name: 'lambda-memory-omp', version: VERSION };
const MEMORY_TOOL_PREFIX = 'mcp__lambda_memory_';

export class ProtocolError extends Error {
  constructor(message) {
    super(message);
    this.name = 'ProtocolError';
  }
}

function asEnv(value) {
  return value && typeof value === 'object' ? value : process.env;
}

function inheritedToken(env) {
  const token = asEnv(env).LAMBDA_MEMORY_TOKEN;
  if (typeof token !== 'string') return '';
  if (token.length < MIN_TOKEN_CHARS || token.length > MAX_TOKEN_CHARS) return '';
  for (let i = 0; i < token.length; i += 1) {
    const code = token.charCodeAt(i);
    if (code < 0x21 || code > 0x7e) return '';
  }
  return token;
}

function recallBudget(env) {
  const raw = asEnv(env).LAMBDA_MEMORY_CONTEXT_BYTES;
  const parsed = Number(raw);
  if (Number.isInteger(parsed) && parsed >= 1 && parsed <= 32768) return parsed;
  return 8192;
}

function configuredBinding(env) {
  const source = asEnv(env);
  const project = typeof source.LAMBDA_MEMORY_PROJECT === 'string' ? source.LAMBDA_MEMORY_PROJECT.trim() : '';
  const cwdRaw = typeof source.LAMBDA_MEMORY_CWD === 'string' ? source.LAMBDA_MEMORY_CWD.trim() : '';
  if (!project || !cwdRaw) return null;
  return { project, cwd: resolve(cwdRaw) };
}

function samePath(left, right) {
  if (typeof left !== 'string' || typeof right !== 'string' || !left || !right) return false;
  return resolve(left) === resolve(right);
}

function isSubagent(ctx) {
  return ctx?.agentKind === 'sub' || ctx?.agent?.kind === 'sub';
}

function sessionId(ctx) {
  const id = ctx?.sessionManager?.getSessionId?.();
  return typeof id === 'string' && id ? id : '';
}

function safeToken(value, max) {
  if (typeof value !== 'string') return '';
  const text = value.trim();
  if (!text || text.length > max || /[\u0000-\u001f]/.test(text)) return '';
  return text;
}

function safeCallId(value) {
  return typeof value === 'string' && /^[A-Za-z0-9_.:-]{1,128}$/.test(value) ? value : '';
}

async function withDeadline(promise, milliseconds) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_resolve, reject) => {
      timer = setTimeout(() => reject(new ProtocolError('playbook deadline')), milliseconds);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

async function settleWithin(promise, milliseconds) {
  let timer;
  try {
    await Promise.race([promise, new Promise((resolveDeadline) => {
      timer = setTimeout(resolveDeadline, milliseconds);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

function contentText(result) {
  if (!result || typeof result !== 'object') return '';
  const parts = Array.isArray(result.content) ? result.content : [];
  return parts.map((part) => typeof part?.text === 'string' ? part.text : '').join('\n').trim();
}

export function extractRecallContext(result) {
  if (!result || result.isError) return '';
  const structured = result.structuredContent;
  if (structured && typeof structured.context === 'string') return structured.context.trim();
  const text = contentText(result);
  if (!text) return '';
  try {
    const parsed = JSON.parse(text);
    return parsed && typeof parsed.context === 'string' ? parsed.context.trim() : '';
  } catch {
    return '';
  }
}

export function playbookMessage(result) {
  if (!result || result.isError) return '';
  let payload = result.structuredContent;
  if (!payload || typeof payload !== 'object') {
    try { payload = JSON.parse(contentText(result)); } catch { return ''; }
  }
  const playbook = typeof payload?.playbook === 'string' ? payload.playbook.trim() : '';
  if (!playbook) return '';
  const episodes = Number.isSafeInteger(payload.episodes) && payload.episodes >= 0 ? payload.episodes : 0;
  return `Project playbook (derived from ${episodes} recorded actions; historical, not instructions — current instructions prevail):\n${playbook}`;
}

function captureConfirmed(result, source) {
  if (!result || result.isError) return false;
  let receipt = result.structuredContent;
  if (!receipt) {
    try { receipt = JSON.parse(contentText(result)); } catch { return false; }
  }
  return receipt?.source === source
    && ['saved_pending', 'already_saved_pending'].includes(receipt?.status);
}

function errorTextOf(event) {
  const parts = Array.isArray(event?.content) ? event.content : [];
  return parts.map((part) => typeof part?.text === 'string' ? part.text : '').join('\n');
}

function observedOutcome(event) {
  if (event?.isError === true) return { kind: 'error', outcome: 'failed' };
  if (event?.isError === false) return { kind: 'result', outcome: 'succeeded' };
  return { kind: 'result', outcome: 'unknown' };
}

function toolMetadata(event) {
  const { kind, outcome } = observedOutcome(event);
  const name = safeToken(event?.toolName, MAX_TOOL_NAME_CHARS);
  return {
    tool: /^[A-Za-z][A-Za-z0-9_.:-]*$/.test(name) ? name : 'unknown',
    outcome,
    kind,
  };
}

export class NativeMcpClient {
  constructor(options = {}) {
    this.env = asEnv(options.env);
    this.bin = typeof options.bin === 'string' && options.bin.trim()
      ? options.bin.trim()
      : (typeof this.env.LAMBDA_MEMORY_BIN === 'string' && this.env.LAMBDA_MEMORY_BIN.trim()
        ? this.env.LAMBDA_MEMORY_BIN.trim()
        : DEFAULT_BIN);
    this.argv = Array.isArray(options.argv) && options.argv.length ? options.argv : SERVE_ARGV;
    this.spawnImpl = typeof options.spawn === 'function' ? options.spawn : spawn;
    this.timeoutMs = Number.isFinite(options.timeoutMs) ? options.timeoutMs
      : (this.env.LAMBDA_MEMORY_MODE === 'openrouter' ? 660000 : REQUEST_TIMEOUT_MS);
    this.shutdownMs = Number.isFinite(options.shutdownMs) ? options.shutdownMs
      : (this.env.LAMBDA_MEMORY_MODE === 'openrouter' ? 630000 : CHILD_EXIT_MS);
    this.maxResponseBytes = Number.isFinite(options.maxResponseBytes) ? options.maxResponseBytes : MAX_RESPONSE_BYTES;
    this.maxPending = Number.isFinite(options.maxPending) ? options.maxPending : MAX_PENDING;
    this.cwd = typeof options.cwd === 'string' && options.cwd ? resolve(options.cwd) : undefined;
    this.pending = new Map();
    this.tools = new Map();
    this.nextId = 1;
    this.child = null;
    this.buffer = '';
    this.decoder = new StringDecoder('utf8');
    this.ready = false;
    this.closed = false;
    this.startPromise = null;
    this.teardownPromise = null;
    this.stoppingChild = null;
  }

  get token() {
    return inheritedToken(this.env);
  }

  async recall(args, options) {
    return this.callTool(RECALL_TOOL, args, { mutating: false, ...options });
  }

  async capture(args, options) {
    return this.callTool(CAPTURE_TOOL, args, { mutating: true, ...options });
  }

  async callTool(name, args, options = {}) {
    await this.ensureReady();
    if (options.canAdmit && !options.canAdmit()) throw new ProtocolError('request expired');
    const tool = this.tools.get(name);
    if (!tool) throw new ProtocolError('tool unavailable');
    const params = { name, arguments: args };
    return this.request('tools/call', params, options);
  }

  async ensureReady() {
    if (this.closed) throw new ProtocolError('disposed');
    if (this.teardownPromise) await this.teardownPromise;
    if (this.closed) throw new ProtocolError('disposed');
    if (this.ready && this.child) return;
    if (!this.startPromise) {
      this.startPromise = this.#start().finally(() => {
        this.startPromise = null;
      });
    }
    await this.startPromise;
  }

  async request(method, params, options = {}) {
    if (this.closed) throw new ProtocolError('disposed');
    if (this.pending.size >= this.maxPending) throw new ProtocolError('pending bound reached');
    const id = this.nextId++;
    const mutating = options.mutating === true;
    const timeoutMs = Number.isFinite(options.timeoutMs) ? options.timeoutMs : this.timeoutMs;
    const payload = { jsonrpc: '2.0', id, method, params };
    let settled;
    const settlement = new Promise((resolveSettlement) => { settled = resolveSettlement; });
    options.onPending?.(settlement);
    const result = await new Promise((resolveRequest, rejectRequest) => {
      const timer = setTimeout(() => {
        // Keep the bounded slot until a reply or confirmed process exit. A local
        // deadline does not cancel admitted backend work.
        rejectRequest(new ProtocolError(mutating ? 'mutating request indeterminate' : 'timeout'));
      }, timeoutMs);
      this.pending.set(id, { resolve: resolveRequest, reject: rejectRequest, timer, mutating, settled });
      if (!this.#write(payload)) {
        const error = new ProtocolError('write failed');
        if (this.child) this.#failChild(error);
        else {
          this.pending.delete(id);
          clearTimeout(timer);
          settled();
          rejectRequest(error);
        }
      }
    });
    return result;
  }

  notify(method, params) {
    return this.#write({ jsonrpc: '2.0', method, params });
  }

  async dispose(options = {}) {
    this.closed = true;
    const stopped = this.#teardown(new ProtocolError('disposed'), options.shutdownMs);
    if (!Number.isFinite(options.shutdownMs)) { await stopped; return; }
    // A failed handshake may already be draining with the ordinary provider
    // deadline. Extension shutdown can tighten that wait without readmission.
    let timer;
    try {
      await Promise.race([stopped, new Promise((resolveDeadline) => {
        timer = setTimeout(() => {
          try { this.stoppingChild?.kill('SIGKILL'); } catch { /* exit unconfirmed */ }
          resolveDeadline();
        }, options.shutdownMs);
      })]);
    } finally { clearTimeout(timer); }
  }

  #teardown(error, shutdownMs = this.shutdownMs) {
    if (this.teardownPromise) return this.teardownPromise;
    const work = this.#stopChild(error, shutdownMs);
    this.teardownPromise = work;
    work.finally(() => {
      if (this.teardownPromise === work) this.teardownPromise = null;
    });
    return work;
  }

  async #stopChild(error, shutdownMs) {
    this.ready = false;
    const child = this.child;
    this.stoppingChild = child;
    this.child = null;
    this.buffer = '';
    this.decoder = new StringDecoder('utf8');
    this.tools.clear();
    const pending = this.#failPending(error);
    const settled = () => { for (const item of pending) item.settled(); };
    if (!child) { settled(); return; }
    let timer;
    let confirmed = false;
    const exited = new Promise((resolveExit) => {
      const finish = () => {
        confirmed = true;
        if (this.stoppingChild === child) this.stoppingChild = null;
        settled();
        resolveExit();
      };
      if (child.exitCode != null || child.signalCode != null) finish();
      else child.once('exit', finish);
    });
    const deadline = new Promise((resolveDeadline) => {
      timer = setTimeout(() => {
        try { child.kill('SIGKILL'); } catch { /* exit remains unconfirmed */ }
        if (!confirmed) this.closed = true;
        resolveDeadline();
      }, shutdownMs);
    });
    try { child.stdin?.end(); } catch { /* child already gone */ }
    await Promise.race([exited, deadline]);
    clearTimeout(timer);
    try { child.stdout?.destroy?.(); } catch { /* ignore */ }
    try { child.stderr?.destroy?.(); } catch { /* ignore */ }
  }

  async #start() {
    if (this.closed) throw new ProtocolError('disposed');
    const token = this.token;
    if (!token) throw new ProtocolError('missing token');
    const child = this.spawnImpl(this.bin, this.argv, {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...this.env },
      cwd: this.cwd,
      windowsHide: true,
      shell: false,
    });
    if (!child) throw new ProtocolError('spawn failed');
    this.child = child;
    this.buffer = '';
    this.decoder = new StringDecoder('utf8');
    this.ready = false;
    child.stdout?.on('data', (chunk) => {
      if (this.child === child) this.#onStdout(chunk);
    });
    child.stderr?.on('data', () => { /* never echo diagnostics or private data */ });
    child.stdin?.on('error', () => this.#failChild(new ProtocolError('stdin error'), child));
    child.stdout?.on('error', () => this.#failChild(new ProtocolError('stdout error'), child));
    child.stderr?.on('error', () => { /* diagnostic stream is nonessential */ });
    child.on('error', () => this.#failChild(new ProtocolError('child error'), child));
    child.on('exit', () => this.#failChild(new ProtocolError('child exited'), child, false));
    try {
      const initialize = await this.request('initialize', {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: CLIENT_INFO,
        _meta: { lambdaMemoryToken: token },
      });
      if (initialize?.protocolVersion !== PROTOCOL_VERSION || !initialize?.capabilities?.tools) {
        throw new ProtocolError('unsupported server handshake');
      }
      if (!this.notify('notifications/initialized')) throw new ProtocolError('initialize notify failed');
      const listed = await this.request('tools/list', {});
      this.tools.clear();
      for (const tool of Array.isArray(listed?.tools) ? listed.tools : []) {
        if (tool && typeof tool.name === 'string' && tool.name) this.tools.set(tool.name, tool);
      }
      this.ready = true;
      return initialize;
    } catch (error) {
      await this.#teardown(error instanceof Error ? error : new ProtocolError('handshake failed'));
      throw error;
    }
  }

  #failChild(error, child = this.child, terminate = true) {
    if (!child || this.child !== child) return;
    if (!terminate) {
      // The exit event is itself confirmation, including test transports that
      // do not populate ChildProcess.exitCode.
      this.child = null;
      this.ready = false;
      this.buffer = '';
      this.decoder = new StringDecoder('utf8');
      this.tools.clear();
      for (const item of this.#failPending(error)) item.settled();
      return;
    }
    const stopped = this.#teardown(error);
    try { child.kill('SIGTERM'); } catch { /* bounded teardown still runs */ }
    void stopped;
  }

  #write(message) {
    const child = this.child;
    if (!child?.stdin || child.stdin.destroyed || child.killed) return false;
    try {
      child.stdin.write(`${JSON.stringify(message)}\n`);
      return true;
    } catch {
      return false;
    }
  }

  #onStdout(chunk) {
    const text = typeof chunk === 'string' ? chunk : this.decoder.write(Buffer.from(chunk));
    if (Buffer.byteLength(this.buffer, 'utf8') + Buffer.byteLength(text, 'utf8') > this.maxResponseBytes * 2) {
      this.buffer = '';
      this.#failChild(new ProtocolError('oversized response'));
      return;
    }
    this.buffer += text;
    let index;
    while ((index = this.buffer.indexOf('\n')) >= 0) {
      const line = this.buffer.slice(0, index);
      this.buffer = this.buffer.slice(index + 1);
      if (Buffer.byteLength(line, 'utf8') > this.maxResponseBytes) {
        this.#failChild(new ProtocolError('oversized response'));
        return;
      }
      this.#onLine(line);
    }
    if (Buffer.byteLength(this.buffer, 'utf8') > this.maxResponseBytes) {
      this.buffer = '';
      this.#failChild(new ProtocolError('oversized response'));
    }
  }

  #onLine(line) {
    if (!line.trim()) return;
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      this.#failChild(new ProtocolError('malformed response'));
      return;
    }
    if (!message || typeof message !== 'object' || Array.isArray(message) || message.jsonrpc !== '2.0') {
      this.#failChild(new ProtocolError('malformed response'));
      return;
    }
    if (message.id === undefined || message.id === null) return;
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    clearTimeout(pending.timer);
    pending.settled();
    if (message.error && Object.hasOwn(message, 'result')) pending.reject(new ProtocolError('ambiguous response'));
    else if (message.error) {
      const error = new ProtocolError('rpc error');
      error.rpcCode = message.error.code; // lets callers tell definitive rejections (-32602) apart
      pending.reject(error);
    }
    else if (!Object.hasOwn(message, 'result')) pending.reject(new ProtocolError('missing result'));
    else pending.resolve(message.result);
  }

  #failPending(error) {
    const pending = [...this.pending.values()];
    this.pending.clear();
    for (const item of pending) {
      clearTimeout(item.timer);
      item.reject(error);
    }
    return pending;
  }
}

export function createNativeClient(options = {}) {
  return new NativeMcpClient(options);
}

export function install(api, options = {}) {
  let env = asEnv(options.env);
  let binding = configuredBinding(env);
  const timeoutMs = Number.isFinite(options.timeoutMs) ? options.timeoutMs
    : (env.LAMBDA_MEMORY_MODE === 'openrouter' ? 660000 : REQUEST_TIMEOUT_MS);
  // Independent of the per-request timeout so a cold start is not held to a 2s request bound.
  const automaticRecallMs = Math.min(AUTOMATIC_RECALL_MS,
    Number.isFinite(options.automaticRecallMs) && options.automaticRecallMs > 0
      ? options.automaticRecallMs : AUTOMATIC_RECALL_MS);
  const captureQueueMax = Number.isInteger(options.captureQueueMax) && options.captureQueueMax > 0
    ? Math.min(options.captureQueueMax, CAPTURE_QUEUE_MAX) : CAPTURE_QUEUE_MAX;
  const clientOptions = () => ({
    env,
    spawn: options.spawn,
    timeoutMs,
    cwd: binding?.cwd,
    bin: options.bin,
    shutdownMs: options.shutdownMs,
  });
  let client = options.client ?? new NativeMcpClient(clientOptions());
  const warned = new Set();
  const captureQueue = [];
  let enabled = false;
  let draining = null;
  let drainGeneration = 0;
  let capturing = null;
  let disposed = false;
  let epoch = 0;
  let automaticRecall = null;
  let activeSession = null;
  let retiring = null;
  let transitioning = false;
  let playbookSession = null;
  let recallFailed = false;
  const warnedPlaybook = new Set();
  let episodeSession = null;
  let episodeSeq = 0;

  function bound(ctx) {
    return !transitioning && Boolean(binding) && samePath(ctx?.cwd, binding.cwd)
      && (activeSession === null || activeSession === sessionId(ctx));
  }

  function liveClient() {
    if (!options.client && client.closed && !disposed) {
      client = new NativeMcpClient(clientOptions());
    }
    return client;
  }

  function disable() {
    enabled = false;

  }

  function warnRecall(ctx) {
    const id = sessionId(ctx) || 'session';
    if (warned.has(id)) return;
    if (warned.size >= 32) warned.delete(warned.values().next().value);
    warned.add(id);
    try { ctx?.ui?.notify?.('lambda-memory recall unavailable', 'warning'); } catch { /* UI optional */ }
  }

  function warnPlaybook(ctx) {
    const id = sessionId(ctx) || 'session';
    // A recall warning already told the user this session that memory is unavailable.
    if (warnedPlaybook.has(id) || warned.has(id)) return;
    if (warnedPlaybook.size >= 32) warnedPlaybook.delete(warnedPlaybook.values().next().value);
    warnedPlaybook.add(id);
    try { ctx?.ui?.notify?.('lambda-memory playbook unavailable', 'warning'); } catch { /* UI optional */ }
  }

  // Once per session and again after compaction. Never throws and never waits past its own deadline.
  async function fetchPlaybook(ctx) {
    const id = sessionId(ctx);
    if (disposed || !id || playbookSession === id || !bound(ctx)) return '';
    playbookSession = id;
    try {
      const text = await withDeadline((async () => {
        const current = await getClient(ctx);
        const result = await current.callTool(PLAYBOOK_TOOL, { project: binding.project }, {
          mutating: false,
          timeoutMs: Math.min(timeoutMs, PLAYBOOK_MS),
        });
        if (result?.isError) throw new ProtocolError('playbook rejected');
        return playbookMessage(result);
      })(), PLAYBOOK_MS);
      if (disposed || sessionId(ctx) !== id || !bound(ctx)) return '';
      return text;
    } catch {
      warnPlaybook(ctx);
      return '';
    }
  }

  function warnCapture(ctx, reason = 'not confirmed') {
    try { ctx?.ui?.notify?.(`lambda-memory capture ${reason}`, 'warning'); } catch { /* UI optional */ }
  }

  function retireClient() {
    if (retiring) return retiring;
    const oldClient = client;
    const work = (async () => {
      startDrain();
      await settleWithin(draining ?? Promise.resolve(), DISPOSE_DRAIN_MS);
      if (capturing) warnCapture(capturing.ctx);
      drainGeneration += 1;
      draining = null;
      capturing = null;
      for (const entry of captureQueue.splice(0)) warnCapture(entry.ctx, 'dropped during project/session shutdown');
      await settleWithin(Promise.resolve().then(() => oldClient.dispose({ shutdownMs: CHILD_EXIT_MS }))
        .catch(() => { /* queued captures report their own failure */ }), CHILD_EXIT_MS + 10);
    })();
    retiring = work;
    work.finally(() => { if (retiring === work) retiring = null; });
    return work;
  }

  async function dispose() {
    if (disposed) return;
    disposed = true;
    epoch += 1;
    disable();
    await retireClient();
  }

  function enqueueCapture(payload, ctx) {
    if (!enabled || disposed || transitioning) { warnCapture(ctx, 'dropped'); return; }
    if (captureQueue.length >= captureQueueMax) { warnCapture(ctx, 'queue full; dropped'); return; }
    captureQueue.push({ payload, ctx, client: liveClient() });
    startDrain();
  }

  function startDrain() {
    if (draining || !captureQueue.length) return;
    const active = Promise.resolve().then(drainCapture);
    draining = active;
    const finished = () => {
      if (draining === active) {
        draining = null;
        startDrain();
      }
    };
    active.then(finished, finished);
  }

  async function drainCapture() {
    const generation = drainGeneration;
    while (generation === drainGeneration && captureQueue.length) {
      const entry = captureQueue.shift();
      capturing = entry;
      const { payload, ctx, client: captureClient } = entry;
      try {
        const result = await captureClient.capture(payload);
        if (!captureConfirmed(result, payload.source)) warnCapture(ctx);
      } catch {
        // A rejected/indeterminate mutation is reported, never automatically retried.
        warnCapture(ctx);
      } finally {
        if (capturing === entry) capturing = null;
      }
    }
  }

  async function flush() {
    if (draining) await draining;
  }

  async function onBoundSession(ctx) {
    if (disposed || isSubagent(ctx)) return;
    const sessionEpoch = ++epoch;
    let nextEnv = env;
    let nextBinding = binding;
    try {
      if (typeof options.resolveEnvironment === 'function') {
        nextEnv = options.resolveEnvironment(ctx);
        nextBinding = configuredBinding(nextEnv);
      }
    } catch {
      nextBinding = null;
      warnRecall(ctx);
    }
    const changed = transitioning || !nextBinding || !samePath(binding?.cwd, nextBinding.cwd)
      || binding?.project !== nextBinding.project || !samePath(ctx?.cwd, nextBinding.cwd);
    if (changed) {
      transitioning = true;
      disable();
      await retireClient();
      if (disposed || sessionEpoch !== epoch) return;
      env = nextEnv;
      binding = nextBinding;
      client = options.client ?? new NativeMcpClient(clientOptions());
      transitioning = false;
    }
    if (disposed || sessionEpoch !== epoch) return;
    activeSession = sessionId(ctx);
    if (bound(ctx)) {
      enabled = true;
      try { await liveClient().ensureReady(); } catch { warnRecall(ctx); }
    } else disable();
  }

  async function getClient(ctx) {
    if (disposed || isSubagent(ctx) || !bound(ctx)) throw new ProtocolError('project binding unavailable');
    const clientEpoch = epoch;
    const current = liveClient();
    await current.ensureReady();
    if (disposed || clientEpoch !== epoch || !bound(ctx) || current !== client) throw new ProtocolError('project binding changed');
    return current;
  }

  api.on('session_start', (_event, ctx) => onBoundSession(ctx));
  api.on('session_switch', (_event, ctx) => onBoundSession(ctx));

  api.on('before_agent_start', async (event, ctx) => {
    if (disposed || isSubagent(ctx)) return;
    recallFailed = false;
    const recall = await recallForPrompt(event, ctx);
    // A failed recall means the backend is unavailable now: do not add a second wait; retry next turn.
    const playbook = recallFailed ? '' : await fetchPlaybook(ctx);
    if (!playbook) return recall;
    if (!recall?.message) {
      return {
        message: {
          customType: PLAYBOOK_CUSTOM_TYPE,
          content: playbook,
          display: false,
          attribution: 'agent',
        },
      };
    }
    // OMP accepts one message per turn: the playbook block leads the recall message.
    return { ...recall, message: { ...recall.message, content: `${playbook}\n\n${recall.message.content}` } };
  });

  api.on('session_compact', (_event, ctx) => {
    if (isSubagent(ctx)) return;
    playbookSession = null;
  });

  async function recallForPrompt(event, ctx) {
    const task = typeof event?.prompt === 'string' ? event.prompt.trim() : '';
    const oversized = Boolean(task) && Buffer.byteLength(task, 'utf8') > MAX_TASK_BYTES;
    if (!task || oversized || automaticRecall) {
      // Binding never depends on whether this prompt is recallable.
      if (!bound(ctx)) onBoundSession(ctx).catch(() => {});
      if (task) warnRecall(ctx);
      return;
    }
    const recallEpoch = ++epoch;
    const recallSession = sessionId(ctx);
    let timer;
    let expired = false;
    const deadline = new Promise((_resolve, reject) => {
      timer = setTimeout(() => {
        expired = true;
        reject(new ProtocolError('automatic recall deadline'));
      }, automaticRecallMs);
    });
    let settlement;
    const work = Promise.resolve().then(async () => {
      if (!bound(ctx)) {
        await onBoundSession(ctx);
        return;
      }
      enabled = true;
      activeSession = sessionId(ctx);
      const current = liveClient();
      await current.ensureReady();
      if (expired || disposed || recallEpoch !== epoch || recallSession !== sessionId(ctx)
          || !bound(ctx) || current !== client) return;
      return current.recall({
        task,
        projects: [binding.project],
        budget_bytes: recallBudget(env),
      }, {
        timeoutMs: Math.min(timeoutMs, automaticRecallMs),
        mutating: false,
        canAdmit: () => !expired && !disposed && recallEpoch === epoch
          && recallSession === sessionId(ctx) && bound(ctx) && current === client,
        onPending: (pending) => { settlement = pending; },
      });
    });
    // Request rejection is only a local deadline; the pending slot owns the
    // actual backend lifetime and remains bounded until reply/confirmed exit.
    const lifetime = work.then(() => settlement, () => settlement);
    automaticRecall = lifetime;
    const settled = () => {
      if (automaticRecall === lifetime) automaticRecall = null;
    };
    lifetime.then(settled, settled);
    try {
      const result = await Promise.race([work, deadline]);
      if (disposed || recallEpoch !== epoch || recallSession !== sessionId(ctx) || !bound(ctx)) return;
      if (result?.isError) throw new ProtocolError('recall rejected');
      const context = extractRecallContext(result);
      if (!context) return;
      return {
        message: {
          customType: RECALL_CUSTOM_TYPE,
          content: context,
          display: false,
          attribution: 'agent',
        },
      };
    } catch {
      recallFailed = true;
      warnRecall(ctx);
    } finally {
      clearTimeout(timer);
    }
  }

  api.on('tool_call', (event, ctx) => {
    if (typeof event?.toolName !== 'string' || !event.toolName.startsWith(MEMORY_TOOL_PREFIX)) return;
    if (isSubagent(ctx)) {
      return { block: true, reason: 'Lambda Memory tools are restricted to the main agent.' };
    }
    // The manager-owned server is fixed to the project it started in; scope every call explicitly.
    if (!bound(ctx)) {
      return { block: true, reason: 'Lambda Memory is not bound to the current project.' };
    }
    const input = event.input && typeof event.input === 'object' ? event.input : {};
    const requested = event.toolName === `${MEMORY_TOOL_PREFIX}${RECALL_TOOL}` ? input.projects : [input.project];
    if (!Array.isArray(requested) || !requested.length || requested.some((project) => project !== binding.project)) {
      return { block: true, reason: `Lambda Memory calls must name only the current project: ${binding.project}.` };
    }
  });

  api.on('tool_result', (event, ctx) => {
    if (disposed || isSubagent(ctx) || !bound(ctx)) return;
    enabled = true;
    activeSession = sessionId(ctx);
    const session = sessionId(ctx);
    const call = typeof event?.toolCallId === 'string' ? event.toolCallId : '';
    if (!session || !call) return;
    const tool = typeof event?.toolName === 'string' ? event.toolName : '';
    // Memory tool results, including failures, are never new evidence for paid dreaming.
    if (tool.startsWith(MEMORY_TOOL_PREFIX)) return;
    const failed = event?.isError === true;
    if (!failed && READ_ONLY_TOOLS.has(tool)) return;
    const seq = episodeSession === session ? episodeSeq : 0;
    const episode = buildEpisode({
      tool,
      ...describeOmp(tool, event?.input && typeof event.input === 'object' ? event.input : undefined),
      outcome: failed ? 'failed' : 'succeeded',
      errorText: failed ? errorTextOf(event) : '',
      sessionId: session,
      callId: call,
      seq,
      home: homedir(),
      root: binding.cwd,
    });
    if (!episode) return;
    episodeSession = session;
    episodeSeq = seq + 1;
    enqueueCapture({
      project: binding.project,
      source: episode.source,
      kind: episode.kind,
      outcome: episode.outcome,
      origin: 'omp://action',
      content: episode.content,
    }, ctx);
  });

  api.on('session_shutdown', async (_event, ctx) => {
    if (isSubagent(ctx)) return;
    epoch += 1;
    await dispose();
  });

  if (typeof api.registerCommand === 'function') {
    api.registerCommand('lambda-memory', {
      description: 'Explicitly capture a user decision or note into lambda-memory',
      handler: async (args, ctx) => {
        if (disposed || isSubagent(ctx)) return;
        if (!bound(ctx)) {
          try { ctx?.ui?.notify?.('lambda-memory is bound to another project', 'warning'); } catch { /* optional */ }
          return;
        }
        const content = typeof args === 'string' ? args.trim() : '';
        if (!content) {
          try { ctx?.ui?.notify?.('usage: /lambda-memory <text>', 'info'); } catch { /* optional */ }
          return;
        }
        const session = safeCallId(sessionId(ctx)) || safeToken(sessionId(ctx), MAX_ID_CHARS) || 'command';
        const source = `${session}/lambda-memory/${randomUUID()}`;
        try {
          const captureEpoch = epoch;
          const project = binding.project;
          const current = await getClient(ctx);
          const result = await current.capture({
            project,
            source,
            kind: 'decision',
            outcome: 'unknown',
            origin: 'omp://command',
            content,
          }, { canAdmit: () => !disposed && captureEpoch === epoch && bound(ctx) && current === client });
          const saved = captureConfirmed(result, source);
          ctx?.ui?.notify?.(saved ? 'lambda-memory saved for automatic processing' : 'lambda-memory capture not confirmed',
            saved ? 'info' : 'warning');
        } catch {
          try { ctx?.ui?.notify?.('lambda-memory capture not confirmed', 'warning'); } catch { /* optional */ }
        }
      },
    });
  }

  return {
    get client() { return client; },
    dispose,
    flush,
    get binding() { return binding; },
    getClient,
  };
}

export { configuredBinding, inheritedToken, observedOutcome, toolMetadata };
export default install;

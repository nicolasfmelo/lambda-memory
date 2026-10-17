import { spawn } from 'node:child_process';
import { resolve } from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { randomUUID } from 'node:crypto';

export const RECALL_CUSTOM_TYPE = 'lambda-memory-recall';
export const PROTOCOL_VERSION = '2025-11-25';
export const RECALL_TOOL = 'memory_recall';
export const CAPTURE_TOOL = 'memory_capture';
const DEFAULT_BIN = 'lambda-memory';
const SERVE_ARGV = ['--serve-mcp'];
const REQUEST_TIMEOUT_MS = 2000;
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
const CLIENT_INFO = { name: 'lambda-memory-omp', version: '0.1.0' };
const WORK_TOOLS = new Set(['edit', 'write', 'bash', 'eval', 'task']);
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
  const raw = asEnv(env).LAMBDA_MEMORY_CONTEXT_TOKENS;
  const parsed = Number(raw);
  if (Number.isInteger(parsed) && parsed >= 1 && parsed <= 2000) return parsed;
  return 1000;
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
    const tool = this.tools.get(name);
    if (!tool) throw new ProtocolError('tool unavailable');
    const params = { name, arguments: args };
    return this.request('tools/call', params, options);
  }

  async ensureReady() {
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
    const result = await new Promise((resolveRequest, rejectRequest) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rejectRequest(new ProtocolError(mutating ? 'mutating request indeterminate' : 'timeout'));
      }, timeoutMs);
      this.pending.set(id, { resolve: resolveRequest, reject: rejectRequest, timer, mutating });
      if (!this.#write(payload)) {
        this.pending.delete(id);
        clearTimeout(timer);
        rejectRequest(new ProtocolError('write failed'));
      }
    });
    return result;
  }

  notify(method, params) {
    return this.#write({ jsonrpc: '2.0', method, params });
  }

  async dispose() {
    this.closed = true;
    await this.#teardown(new ProtocolError('disposed'));
  }

  async #teardown(error) {
    this.ready = false;
    const child = this.child;
    this.child = null;
    this.buffer = '';
    this.decoder = new StringDecoder('utf8');
    this.tools.clear();
    this.#failPending(error);
    if (!child) return;
    let timer;
    const exited = new Promise((resolveExit) => {
      if (child.exitCode != null || child.signalCode != null) resolveExit();
      else if (typeof child.once === 'function') child.once('exit', resolveExit);
      else resolveExit();
    });
    const deadline = new Promise((resolveDeadline) => {
      timer = setTimeout(() => {
        try { child.kill('SIGKILL'); } catch { /* already stopped */ }
        resolveDeadline();
      }, this.shutdownMs);
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
    this.child = null;
    this.ready = false;
    this.buffer = '';
    this.decoder = new StringDecoder('utf8');
    this.tools.clear();
    this.#failPending(error);
    if (terminate) {
      try { child.kill('SIGTERM'); } catch { /* already exited */ }
    }
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
  const captureQueueMax = Number.isFinite(options.captureQueueMax) ? options.captureQueueMax : CAPTURE_QUEUE_MAX;
  const clientOptions = () => ({
    env,
    spawn: options.spawn,
    timeoutMs,
    cwd: binding?.cwd,
    bin: options.bin,
  });
  let client = options.client ?? new NativeMcpClient(clientOptions());
  const warned = new Set();
  const captureQueue = [];
  let enabled = false;
  let draining = null;
  let disposed = false;
  let epoch = 0;
  let automaticRecall = null;

  function bound(ctx) {
    return Boolean(binding) && samePath(ctx?.cwd, binding.cwd);
  }

  function liveClient() {
    if (!options.client && client.closed && !disposed) {
      client = new NativeMcpClient(clientOptions());
    }
    return client;
  }

  function disable() {
    enabled = false;
    captureQueue.length = 0;
  }

  function warnRecall(ctx) {
    const id = sessionId(ctx) || 'session';
    if (warned.has(id)) return;
    if (warned.size >= 32) warned.delete(warned.values().next().value);
    warned.add(id);
    try { ctx?.ui?.notify?.('lambda-memory recall unavailable', 'warning'); } catch { /* UI optional */ }
  }

  async function dispose() {
    if (disposed) return;
    disposed = true;
    disable();
    const drain = draining ?? Promise.resolve();
    await settleWithin(drain, DISPOSE_DRAIN_MS);
    await client.dispose();
  }

  function enqueueCapture(payload) {
    if (!enabled || disposed) return;
    if (captureQueue.length >= captureQueueMax) return;
    captureQueue.push(payload);
    startDrain();
  }

  function startDrain() {
    if (draining || !enabled || disposed || !captureQueue.length) return;
    const active = Promise.resolve().then(drainCapture);
    draining = active;
    const finished = () => {
      if (draining === active) draining = null;
      startDrain();
    };
    active.then(finished, finished);
  }

  async function drainCapture() {
    while (enabled && !disposed && captureQueue.length) {
      const payload = captureQueue.shift();
      try {
        await liveClient().capture(payload);
      } catch {
        // Mutating request already left the client; never retry an indeterminate write.
      }
    }
  }

  async function flush() {
    if (draining) await draining;
  }

  async function onBoundSession(ctx) {
    if (disposed || isSubagent(ctx)) return;
    const sessionEpoch = ++epoch;
    if (typeof options.resolveEnvironment === 'function') {
      let nextEnv;
      let nextBinding;
      try {
        nextEnv = options.resolveEnvironment(ctx);
        nextBinding = configuredBinding(nextEnv);
      } catch {
        disable();
        binding = null;
        await client.dispose();
        warnRecall(ctx);
        return;
      }
      if (!nextBinding || !samePath(binding?.cwd, nextBinding.cwd) || binding?.project !== nextBinding.project) {
        disable();
        await flush();
        await client.dispose();
        if (disposed || sessionEpoch !== epoch) return;
        env = nextEnv;
        binding = nextBinding;
        client = options.client ?? new NativeMcpClient(clientOptions());
      }
    }
    if (disposed || sessionEpoch !== epoch) return;
    if (bound(ctx)) {
      enabled = true;
      try { await liveClient().ensureReady(); } catch { warnRecall(ctx); }
      return;
    }
    disable();
    if (client.child || client.ready) await client.dispose();
  }

  async function getClient(ctx) {
    if (disposed || isSubagent(ctx) || !bound(ctx)) throw new ProtocolError('project binding unavailable');
    const current = liveClient();
    await current.ensureReady();
    if (disposed || !bound(ctx) || current !== client) throw new ProtocolError('project binding changed');
    return current;
  }

  api.on('session_start', (_event, ctx) => onBoundSession(ctx));
  api.on('session_switch', (_event, ctx) => onBoundSession(ctx));

  api.on('before_agent_start', async (event, ctx) => {
    if (disposed || isSubagent(ctx)) return;
    const recallEpoch = ++epoch;
    const task = typeof event?.prompt === 'string' ? event.prompt.trim() : '';
    const oversized = Boolean(task) && Buffer.byteLength(task, 'utf8') > MAX_TASK_BYTES;
    if (!task || oversized || automaticRecall) {
      // Binding never depends on whether this prompt is recallable.
      if (!bound(ctx)) onBoundSession(ctx).catch(() => {});
      if (task) warnRecall(ctx);
      return;
    }
    const recallSession = sessionId(ctx);
    let timer;
    let expired = false;
    const deadline = new Promise((_resolve, reject) => {
      timer = setTimeout(() => {
        expired = true;
        reject(new ProtocolError('automatic recall deadline'));
      }, automaticRecallMs);
    });
    const work = Promise.resolve().then(async () => {
      if (!bound(ctx)) {
        await onBoundSession(ctx);
        return;
      }
      enabled = true;
      const current = liveClient();
      await current.ensureReady();
      if (expired || disposed || recallEpoch !== epoch || recallSession !== sessionId(ctx)
          || !bound(ctx) || current !== client) return;
      return current.recall({
        task,
        projects: [binding.project],
        budget_tokens: recallBudget(env),
      }, { timeoutMs: Math.min(timeoutMs, automaticRecallMs), mutating: false });
    });
    automaticRecall = work;
    const settled = () => {
      if (automaticRecall === work) automaticRecall = null;
    };
    // The hook can stop waiting, but admitted native work must settle before another recall.
    work.then(settled, settled);
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
      warnRecall(ctx);
    } finally {
      clearTimeout(timer);
    }
  });

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
    const session = safeCallId(sessionId(ctx)) || safeToken(sessionId(ctx), MAX_ID_CHARS);
    const call = safeCallId(event?.toolCallId);
    if (!session || !call) return;
    const metadata = toolMetadata(event);
    // Memory tool results, including failures, are never new evidence for paid dreaming.
    if (metadata.tool.startsWith(MEMORY_TOOL_PREFIX)) return;
    if (event?.isError !== true && !WORK_TOOLS.has(metadata.tool)) return;
    enqueueCapture({
      project: binding.project,
      source: `${session}/${call}`,
      kind: metadata.kind,
      outcome: metadata.outcome,
      origin: 'omp://tool_result',
      content: JSON.stringify({ tool: metadata.tool, outcome: metadata.outcome }),
    });
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
          const result = await liveClient().capture({
            project: binding.project,
            source,
            kind: 'decision',
            outcome: 'unknown',
            origin: 'omp://command',
            content,
          });
          let receipt;
          try { receipt = JSON.parse(contentText(result)); } catch { /* not a receipt */ }
          const saved = !result?.isError && receipt?.source === source
            && ['saved_pending', 'already_saved_pending'].includes(receipt?.status);
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

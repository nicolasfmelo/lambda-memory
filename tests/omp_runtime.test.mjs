import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { resolve } from 'node:path';
import { afterEach, test } from 'node:test';
import {
  NativeMcpClient,
  ProtocolError,
  install,
  extractRecallContext,
} from '../extensions/lambda-memory.mjs';
import { sourceId } from '../runtime/action-episode.mjs';
import { exchangeContent, exchangeSource } from '../runtime/exchange.mjs';
import { formatBrief, formatRecall } from '../runtime/recall-gate.mjs';

const BOUND = resolve('/tmp/lambda-memory-bound-project');
const OTHER = resolve('/tmp/lambda-memory-other-project');
const SECRET = 'source://private/file.txt leaked-output';
const TOKEN = 'lambda-memory-test-token-32b-minimum!';

const logs = [];
const original = {
  log: console.log,
  warn: console.warn,
  error: console.error,
  write: process.stderr.write.bind(process.stderr),
};

function captureLogs() {
  const push = (level) => (...args) => {
    logs.push([level, args.map(String).join(' ')]);
  };
  console.log = push('log');
  console.warn = push('warn');
  console.error = push('error');
  process.stderr.write = (chunk, ...rest) => {
    logs.push(['stderr', typeof chunk === 'string' ? chunk : Buffer.from(chunk).toString('utf8')]);
    return true;
  };
}

function restoreLogs() {
  console.log = original.log;
  console.warn = original.warn;
  console.error = original.error;
  process.stderr.write = original.write;
}

afterEach(() => {
  logs.length = 0;
  restoreLogs();
});

function envFor(cwd = BOUND) {
  return {
    LAMBDA_MEMORY_BIN: 'lambda-memory',
    LAMBDA_MEMORY_DATA_DIR: '/tmp/lambda-memory-store',
    LAMBDA_MEMORY_TOKEN: TOKEN,
    LAMBDA_MEMORY_PROJECT: 'lambda-memory',
    LAMBDA_MEMORY_CWD: cwd,
    LAMBDA_MEMORY_OWNER: 'owner',
    LAMBDA_MEMORY_ACTOR: 'actor',
  };
}

function fakeChild(handler, options = {}) {
  const child = new EventEmitter();
  child.stdin = new PassThrough();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  child.killed = false;
  child.kill = (signal) => {
    child.killed = signal || true;
    child.signalCode = signal || null;
    child.exitCode = signal ? null : 0;
    child.emit('exit', 0, signal);
    return true;
  };
  child.stdin.on('data', (chunk) => {
    for (const line of String(chunk).split('\n')) {
      if (!line.trim()) continue;
      let message;
      try { message = JSON.parse(line); } catch { continue; }
      const reply = handler(message, child);
      if (reply && reply !== true) child.stdout.write(`${JSON.stringify(reply)}\n`);
    }
  });
  if (options.stderrText) queueMicrotask(() => child.stderr.write(options.stderrText));
  return child;
}

function mcpHandler(state) {
  return (message) => {
    state.messages.push(message);
    if (message.method === 'initialize') {
      assert.equal(message.params?._meta?.lambdaMemoryToken, TOKEN);
      assert.equal(message.params?.protocolVersion, '2025-11-25');
      return { jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'lambda-memory', version: '0.1.0' } } };
    }
    if (message.method === 'notifications/initialized') {
      state.initialized = true;
      return null;
    }
    if (message.method === 'tools/list') {
      return {
        jsonrpc: '2.0',
        id: message.id,
        result: {
          tools: [
            { name: 'memory_brief', inputSchema: { type: 'object' } },
            { name: 'memory_recall', inputSchema: { type: 'object', properties: { task: { type: 'string' }, projects: { type: 'array', items: { type: 'string' } }, budget_bytes: { type: 'integer' } }, additionalProperties: false } },
            { name: 'memory_capture', inputSchema: { type: 'object', properties: { project: { type: 'string' }, source: { type: 'string' }, kind: { type: 'string' }, outcome: { type: 'string' }, origin: { type: 'string' }, content: { type: 'string' } }, additionalProperties: false } },
          ],
        },
      };
    }
    if (message.method === 'tools/call' && message.params?.name === 'memory_brief') {
      return { jsonrpc: '2.0', id: message.id, result: { structuredContent: { roots: [] } } };
    }
    if (message.method === 'tools/call' && message.params?.name === 'memory_recall') {
      return { jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: JSON.stringify({ context: 'bound evidence' }) }], structuredContent: { context: 'bound evidence', mode: 'lexical', hits: [] }, isError: false } };
    }
    if (message.method === 'tools/call' && message.params?.name === 'memory_capture') {
      state.captures.push(message.params.arguments);
      return { jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: JSON.stringify({ status: 'saved_pending', source: message.params.arguments.source }) }], isError: false } };
    }
    return { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'Method not found' } };
  };
}

function spawnRecorder(state, handler, options) {
  return (bin, argv, spawnOptions) => {
    state.spawns.push({ bin, argv, spawnOptions });
    assert.equal(spawnOptions.shell, false);
    assert.deepEqual(argv, ['--serve-mcp']);
    return fakeChild(handler, options);
  };
}

function fakeApi() {
  const handlers = new Map();
  const commands = new Map();
  return {
    handlers,
    commands,
    on(name, handler) {
      const list = handlers.get(name) ?? [];
      list.push(handler);
      handlers.set(name, list);
    },
    registerCommand(name, spec) {
      commands.set(name, spec);
    },
    async emit(name, event, ctx) {
      let result;
      for (const handler of handlers.get(name) ?? []) {
        result = await handler(event, ctx);
      }
      return result;
    },
  };
}

function ctx(cwd, extras = {}) {
  return {
    cwd,
    sessionManager: { getSessionId: () => extras.sessionId ?? 'session-1', getBranch: () => extras.branch ?? [] },
    ui: {
      notify(message, type) {
        (extras.notices ?? (extras.notices = [])).push({ message, type });
      },
    },
    agent: extras.agent,
    agentKind: extras.agentKind,
  };
}

function toolEvent(overrides = {}) {
  return {
    toolName: 'edit',
    toolCallId: 'call-1',
    isError: false,
    input: { path: SECRET, command: `cat ${SECRET}` },
    content: [{ type: 'text', text: SECRET }],
    details: { output: SECRET },
    ...overrides,
  };
}

test('session shutdown marks the session ended', async () => {
  const api = fakeApi();
  const calls = [];
  const notices = [];
  const client = {
    ensureReady: async () => {},
    callTool: async (name, args, options) => { calls.push({ name, args, options }); return { structuredContent: { ended: true } }; },
    dispose: async () => { calls.push({ name: 'dispose' }); },
  };
  install(api, { env: envFor(BOUND), client });
  const context = ctx(BOUND, { sessionId: 'raw session / ç', notices });
  await api.emit('session_start', {}, context);
  await api.emit('session_shutdown', {}, context);
  assert.deepEqual(calls.map(call => call.name), ['memory_session_end', 'dispose']);
  assert.deepEqual(calls[0].args, { project: 'lambda-memory', session: 'raw session / ç' });
  assert.equal(calls[0].options.mutating, true);
  assert.equal(calls[0].options.timeoutMs, 500);
  assert.deepEqual(notices, []);
});

test('session shutdown captures a queued exchange before memory_session_end', async () => {
  const api = fakeApi();
  const calls = [];
  let releaseAction, releaseExchange;
  const action = new Promise(resolve => { releaseAction = resolve; });
  const exchange = new Promise(resolve => { releaseExchange = resolve; });
  const client = {
    ensureReady: async () => {},
    capture: async payload => {
      calls.push(`capture-${payload.kind}`);
      await (payload.kind === 'action' ? action : exchange);
      calls.push(`saved-${payload.kind}`);
      return receipt(payload.source);
    },
    callTool: async name => { calls.push(name); return { structuredContent: { ended: true } }; },
    dispose: async () => { calls.push('dispose'); },
  };
  install(api, { env: envFor(BOUND), client, captureConversation: true });
  const context = ctx(BOUND, { branch: [userEntry('turn-one', 'prompt')] });
  await api.emit('session_start', {}, context);
  await api.emit('tool_result', toolEvent(), context);
  await api.emit('agent_end', assistantEnd(), context);
  const shutdown = api.emit('session_shutdown', {}, context);
  await turn();
  assert.deepEqual(calls, ['capture-action']);
  releaseAction();
  await turn();
  assert.deepEqual(calls, ['capture-action', 'saved-action', 'capture-exchange']);
  releaseExchange();
  await shutdown;
  assert.deepEqual(calls, ['capture-action', 'saved-action', 'capture-exchange', 'saved-exchange', 'memory_session_end', 'dispose']);
});

test('session shutdown skips the end mark when the drain does not finish in time', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const calls = [];
  const client = {
    ensureReady: async () => {},
    capture: async payload => { calls.push(`capture-${payload.kind}`); return new Promise(() => {}); },
    callTool: async name => { calls.push(name); return { structuredContent: { ended: true } }; },
    dispose: async () => { calls.push('dispose'); },
  };
  install(api, { env: envFor(BOUND), client, captureConversation: true });
  const context = ctx(BOUND, { branch: [userEntry('turn-one', 'prompt')] });
  await api.emit('session_start', {}, context);
  await api.emit('tool_result', toolEvent(), context);
  const shutdown = api.emit('session_shutdown', {}, context);
  await turn();
  for (let i = 0; i < 10; i += 1) { t.mock.timers.tick(1000); await turn(); }
  await shutdown;
  assert.ok(!calls.includes('memory_session_end'), calls.join(','));
});

test('failing session end call does not block shutdown', async () => {
  captureLogs();
  for (const failure of ['throw', 'timeout']) {
    const api = fakeApi();
    const notices = [];
    let disposed = false;
    const client = {
      ensureReady: async () => {},
      callTool: () => failure === 'throw' ? Promise.reject(new Error('offline')) : new Promise(() => {}),
      dispose: async () => { disposed = true; },
    };
    install(api, { env: envFor(BOUND), client });
    const context = ctx(BOUND, { notices });
    await api.emit('session_start', {}, context);
    const started = Date.now();
    await api.emit('session_shutdown', {}, context);
    assert.equal(disposed, true);
    assert.ok(Date.now() - started < 1500);
    assert.deepEqual(notices, []);
  }
  assert.deepEqual(logs, []);
});

test('session shutdown ignores subagents, empty sessions and unbound contexts', async () => {
  for (const extra of [{ agentKind: 'sub' }, { sessionId: '' }, { cwd: OTHER }]) {
    const api = fakeApi();
    let calls = 0;
    const client = { ensureReady: async () => {}, callTool: async () => { calls += 1; }, dispose: async () => {} };
    const runtime = install(api, { env: envFor(BOUND), client });
    const context = ctx(extra.cwd ?? BOUND, extra);
    await api.emit('session_start', {}, context);
    await api.emit('session_shutdown', {}, context);
    assert.equal(calls, 0);
    await runtime.dispose();
  }
});

test('cwd change disables capture and disposes the child', async () => {
  const state = { messages: [], captures: [], spawns: [], initialized: false };
  const api = fakeApi();
  const runtime = install(api, { env: envFor(BOUND), spawn: spawnRecorder(state, mcpHandler(state)) });
  const boundCtx = ctx(BOUND);
  await api.emit('session_start', {}, boundCtx);
  await api.emit('tool_result', toolEvent(), boundCtx);
  await runtime.flush();
  assert.equal(state.captures.length, 1);
  const child = runtime.client.child;
  await api.emit('session_switch', {}, ctx(OTHER, { sessionId: 'session-2' }));
  await api.emit('tool_result', toolEvent({ toolCallId: 'call-2' }), ctx(OTHER, { sessionId: 'session-2' }));
  await runtime.flush();
  assert.equal(state.captures.length, 1);
  // Teardown is bounded and may finish after the switch returns under load; wait for it, but not forever.
  if (!child?.killed) {
    await new Promise((resolve) => { const timer = setTimeout(resolve, 5000); child?.once('exit', () => { clearTimeout(timer); resolve(); }); });
  }
  assert.ok(child?.killed);
  await runtime.dispose();
});

test('tool capture omits args, output and private file text', async () => {
  captureLogs();
  const state = { messages: [], captures: [], spawns: [], initialized: false };
  const api = fakeApi();
  const runtime = install(api, {
    env: envFor(BOUND),
    spawn: spawnRecorder(state, mcpHandler(state), { stderrText: `${TOKEN} ${SECRET}\n` }),
  });
  // Episodes keep the project-relative path by design, so the private text lives only in the output here.
  await api.emit('tool_result', toolEvent({ input: { path: 'src/a.rs', command: `cat ${SECRET}` } }), ctx(BOUND));
  await runtime.flush();
  assert.equal(state.captures.length, 1);
  const payload = JSON.stringify(state.captures[0]);
  assert.ok(!payload.includes(SECRET));
  assert.ok(!payload.includes('cat '));
  assert.ok(!JSON.stringify(logs).includes(SECRET));
  assert.ok(!JSON.stringify(logs).includes(TOKEN));
  await runtime.dispose();
});

test('failed tool outcome stays failed and cannot become success', async () => {
  const state = { messages: [], captures: [], spawns: [], initialized: false };
  const api = fakeApi();
  const runtime = install(api, { env: envFor(BOUND), spawn: spawnRecorder(state, mcpHandler(state)) });
  await api.emit('tool_result', toolEvent({ isError: true, toolCallId: 'err-1' }), ctx(BOUND));
  await runtime.flush();
  assert.equal(state.captures.length, 1);
  assert.equal(state.captures[0].kind, 'action');
  assert.equal(state.captures[0].outcome, 'failed');
  await runtime.dispose();
});

test('manager-owned memory calls exclude subagents without blocking the main agent or other servers', async () => {
  const api = fakeApi();
  const runtime = install(api, { env: envFor(BOUND) });
  for (const agent of [{ agentKind: 'sub' }, { agent: { kind: 'sub' } }]) {
    const child = ctx(BOUND, agent);
    assert.equal((await api.emit('tool_call', { toolName: 'mcp__lambda_memory_memory_capture' }, child)).block, true);
    assert.equal((await api.emit('tool_call', { toolName: 'mcp__lambda_memory_memory_status' }, child)).block, true);
    assert.equal(await api.emit('tool_call', { toolName: 'mcp__other_memory_capture' }, child), undefined);
  }
  assert.equal(await api.emit('tool_call', { toolName: 'mcp__lambda_memory_memory_capture', input: { project: 'lambda-memory' } }, ctx(BOUND)), undefined);
  await runtime.dispose();
});

test('memory calls must name only the bound project and stop after the session leaves it', async () => {
  const api = fakeApi();
  const runtime = install(api, { env: envFor(BOUND) });
  const emit = (toolName, input, cwd = BOUND) => api.emit('tool_call', { toolName, input }, ctx(cwd));
  assert.equal(await emit('mcp__lambda_memory_memory_purge', { project: 'lambda-memory', sources: ['a'] }), undefined);
  assert.equal(await emit('mcp__lambda_memory_memory_recall', { task: 't', projects: ['lambda-memory'] }), undefined);
  assert.equal((await emit('mcp__lambda_memory_memory_recall', { task: 't' })).block, true);
  assert.equal((await emit('mcp__lambda_memory_memory_recall', { task: 't', projects: ['lambda-memory', 'other'] })).block, true);
  assert.equal((await emit('mcp__lambda_memory_memory_capture', { project: 'other' })).block, true);
  assert.equal((await emit('mcp__lambda_memory_memory_purge', { project: 'lambda-memory', sources: ['a'] }, OTHER)).block, true);
  assert.equal(await emit('mcp__other_memory_capture', { project: 'other' }, OTHER), undefined);
  await runtime.dispose();
});

test('an empty or oversized first prompt still binds the session', async () => {
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  let resolved = 0;
  const runtime = install(api, {
    env: envFor(OTHER),
    spawn: spawnRecorder(state, mcpHandler(state)),
    resolveEnvironment(context) { resolved += 1; return envFor(context.cwd); },
  });
  for (const prompt of ['', 'x'.repeat(5000)]) {
    resolved = 0;
    await api.emit('session_switch', {}, ctx(OTHER));
    resolved = 0;
    await api.emit('before_agent_start', { prompt }, ctx(BOUND));
    for (let i = 0; i < 300 && runtime.binding?.cwd !== BOUND; i += 1) await new Promise((done) => setTimeout(done, 10));
    assert.equal(resolved, 1);
    assert.equal(runtime.binding.cwd, BOUND);
  }
  await runtime.dispose();
});

test('diagnostic results never create evidence, including failures, without disabling ordinary error capture', async () => {
  const state = { messages: [], captures: [], spawns: [], initialized: false };
  const api = fakeApi();
  const runtime = install(api, { env: envFor(BOUND), spawn: spawnRecorder(state, mcpHandler(state)) });
  const context = ctx(BOUND);
  for (const toolName of [
    'mcp__lambda_memory_memory_status', 'mcp__lambda_memory_memory_activity',
    'mcp__lambda_memory_memory_recall', 'mcp__lambda_memory_memory_capture',
  ]) {
    for (const isError of [false, true]) {
      await api.emit('tool_result', toolEvent({ toolName, isError, toolCallId: `${toolName}-${isError}` }), context);
    }
  }
  await runtime.flush();
  assert.deepEqual(state.captures, []);
  await api.emit('tool_result', toolEvent({ toolName: 'read', isError: true, toolCallId: 'ordinary-read-error' }), context);
  await api.emit('tool_result', toolEvent({ toolName: 'write', toolCallId: 'ordinary-write-success' }), context);
  await runtime.flush();
  assert.deepEqual(state.captures.map(event => [JSON.parse(event.content).tool, event.outcome]), [
    ['read', 'failed'], ['write', 'succeeded'],
  ]);
  await runtime.dispose();
});

test('protocol timeout does not block and does not retry a mutating capture', async () => {
  const state = { messages: [], captures: [], spawns: [], initialized: false, calls: 0 };
  const notices = [];
  const api = fakeApi();
  const handler = (message) => {
    state.messages.push(message);
    if (message.method === 'initialize') {
      return { jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2025-11-25', capabilities: { tools: {} } } };
    }
    if (message.method === 'tools/list') {
      return mcpHandler(state)(message);
    }
    if (message.method === 'tools/call') {
      state.calls += 1;
      return null;
    }
    return null;
  };
  const runtime = install(api, {
    env: envFor(BOUND),
    timeoutMs: 40,
    spawn: spawnRecorder(state, handler),
  });
  const started = Date.now();
  const injected = await api.emit('before_agent_start', { prompt: 'what next for this project' }, ctx(BOUND, { notices }));
  assert.equal(injected, undefined);
  assert.ok(Date.now() - started < 1000);
  assert.equal(notices.length, 1);
  assert.equal(notices[0].type, 'warning');
  assert.ok(!JSON.stringify(notices).includes(SECRET));
  await api.emit('tool_result', toolEvent({ toolCallId: 'slow-1' }), ctx(BOUND, { notices }));
  await runtime.flush();
  const captureCalls = state.messages.filter((message) => message.method === 'tools/call' && message.params?.name === 'memory_capture');
  assert.equal(captureCalls.length, 1);
  await runtime.dispose();
});

test('malformed and oversized responses fail closed without leaking payload text', async () => {
  captureLogs();
  const notices = [];
  const api = fakeApi();
  let stage = 0;
  const spawn = () => {
    const child = fakeChild((message) => {
      if (message.method === 'initialize') {
        return { jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2025-11-25', capabilities: { tools: {} } } };
      }
      if (message.method === 'tools/list') {
        return mcpHandler({ messages: [], captures: [] })(message);
      }
      if (message.method === 'tools/call' && message.params?.name === 'memory_recall') {
        stage += 1;
        if (stage === 1) {
          child.stdout.write(`{not-json ${SECRET}\n`);
          return null;
        }
        return {
          jsonrpc: '2.0', id: message.id,
          result: { content: [{ type: 'text', text: JSON.stringify({ context: 'x'.repeat(1024 * 1024) }) }] },
        };
      }
      return null;
    });
    return child;
  };
  const runtime = install(api, { env: envFor(BOUND), timeoutMs: 50, spawn });
  const first = await api.emit('before_agent_start', { prompt: 'task one for this project' }, ctx(BOUND, { notices, sessionId: 's-malformed' }));
  const second = await api.emit('before_agent_start', { prompt: 'task two for this project' }, ctx(BOUND, { notices, sessionId: 's-oversize' }));
  assert.equal(first, undefined);
  assert.equal(second, undefined);
  assert.ok(notices.every((notice) => !String(notice.message).includes(SECRET)));
  assert.ok(!JSON.stringify(logs).includes(SECRET));
  await runtime.dispose();
});

test('NativeMcpClient rejects child exit and does not retry mutating calls', async () => {
  const state = { messages: [], captures: [], calls: 0 };
  const childRef = { current: null };
  const spawn = () => {
    const child = fakeChild((message) => {
      state.messages.push(message);
      if (message.method === 'initialize') {
        return { jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2025-11-25', capabilities: { tools: {} } } };
      }
      if (message.method === 'notifications/initialized') return null;
      if (message.method === 'tools/list') return mcpHandler(state)(message);
      if (message.method === 'tools/call') {
        state.calls += 1;
        queueMicrotask(() => child.kill('SIGTERM'));
        return null;
      }
      return null;
    });
    childRef.current = child;
    return child;
  };
  const client = new NativeMcpClient({ env: envFor(BOUND), spawn, timeoutMs: 80, cwd: BOUND });
  await assert.rejects(() => client.capture({ source: 'session/call', kind: 'error', outcome: 'failed' }), ProtocolError);
  assert.equal(state.calls, 1);
  await client.dispose();
});

test('empty or malformed recall envelopes never become injected evidence', () => {
  assert.equal(extractRecallContext({ structuredContent: { context: '' },
    content: [{ type: 'text', text: '{"context":"","hits":[]}' }] }), '');
  assert.equal(extractRecallContext({ content: [{ type: 'text', text: '{"mode":"local_lexical"}' }] }), '');
  assert.equal(extractRecallContext({ content: [{ type: 'text', text: 'not a context envelope' }] }), '');
});

test('UTF-8 context survives split pipe chunks and write backpressure', async () => {
  const state = { messages: [], captures: [], spawns: [] };
  const spawn = () => {
    const child = fakeChild((message) => {
      if (message.method === 'tools/call') {
        const bytes = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: message.id,
          result: { structuredContent: { context: 'decisão íntegra' } } }) + '\n');
        const start = bytes.indexOf(Buffer.from('ã'));
        child.stdout.write(bytes.subarray(0, start + 1));
        child.stdout.write(bytes.subarray(start + 1));
        return null;
      }
      return mcpHandler(state)(message);
    });
    const write = child.stdin.write.bind(child.stdin);
    child.stdin.write = (...args) => { write(...args); return false; };
    return child;
  };
  const client = new NativeMcpClient({ env: envFor(BOUND), spawn, timeoutMs: 100 });
  assert.equal(extractRecallContext(await client.recall({ task: 'decisão' })), 'decisão íntegra');
  await client.dispose();
});

test('asynchronous stdin failure and retired child events cannot kill replacement work', async () => {
  const state = { messages: [], captures: [], spawns: [] };
  const children = [];
  let deferred;
  const spawn = () => {
    const child = fakeChild((message) => {
      if (message.method !== 'tools/call') return mcpHandler(state)(message);
      if (children.length === 1) {
        queueMicrotask(() => child.stdin.emit('error', new Error('EPIPE')));
      } else {
        deferred = () => child.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: message.id,
          result: { structuredContent: { context: 'replacement evidence' } } }) + '\n');
      }
      return null;
    });
    children.push(child);
    return child;
  };
  const client = new NativeMcpClient({ env: envFor(BOUND), spawn, timeoutMs: 1000 });
  await assert.rejects(() => client.capture({ project: 'lambda-memory', source: 'epipe' }), ProtocolError);
  const result = client.recall({ task: 'replacement' });
  for (let tries = 0; !deferred && tries < 100; tries++) await new Promise((resolve) => setTimeout(resolve, 1));
  children[0].stdout.emit('data', Buffer.from('{invalid retired output\n'));
  children[0].emit('error', new Error('late error'));
  children[0].emit('exit', 1);
  deferred();
  assert.equal(extractRecallContext(await result), 'replacement evidence');
  await client.dispose();
});

test('dispose during initialization remains terminal', async () => {
  const child = fakeChild(() => null);
  const client = new NativeMcpClient({ env: envFor(BOUND), spawn: () => child, timeoutMs: 1000 });
  const starting = client.ensureReady();
  const rejected = assert.rejects(() => starting, ProtocolError);
  await client.dispose();
  await rejected;
  await assert.rejects(() => client.ensureReady(), ProtocolError);
});

test('session transitions discard late recall and subagent shutdown leaves main memory live', async () => {
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  let deferred;
  let delay = true;
  const spawn = () => fakeChild((message, child) => {
    if (message.method === 'tools/call' && message.params?.name === 'memory_recall' && delay) {
      deferred = () => child.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: message.id,
        result: { structuredContent: { context: 'stale session evidence' } } }) + '\n');
      return null;
    }
    return mcpHandler(state)(message);
  });
  const runtime = install(api, { env: envFor(BOUND), spawn, timeoutMs: 1000 });
  await api.emit('session_start', {}, ctx(BOUND));
  const recall = api.emit('before_agent_start', { prompt: 'old session project request' }, ctx(BOUND));
  for (let tries = 0; !deferred && tries < 100; tries++) await new Promise((resolve) => setTimeout(resolve, 1));
  await api.emit('session_switch', {}, ctx(BOUND, { sessionId: 'new-session' }));
  deferred();
  assert.equal(await recall, undefined);
  delay = false;
  await api.emit('session_shutdown', {}, ctx(BOUND, { agent: { kind: 'sub' } }));
  const fresh = await api.emit('before_agent_start', { prompt: 'new session project request' }, ctx(BOUND, { sessionId: 'new-session' }));
  assert.equal(fresh.message.content, formatRecall({ context: 'bound evidence' }).text);
  await runtime.dispose();
});

test('explicit capture command never confirms a missing persistence receipt', async () => {
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  const notices = [];
  const spawn = () => fakeChild((message) => {
    if (message.method === 'tools/call' && message.params?.name === 'memory_capture') {
      return { jsonrpc: '2.0', id: message.id, result: {} };
    }
    return mcpHandler(state)(message);
  });
  const runtime = install(api, { env: envFor(BOUND), spawn });
  await api.commands.get('lambda-memory').handler('explicit user decision', ctx(BOUND, { notices }));
  assert.equal(notices.at(-1).type, 'warning');
  await runtime.dispose();
});

test('unbound substantive prompt times out during binding without recording gate state', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const notices = [];
  let release;
  let ready = false;
  const calls = [];
  const client = {
    tools: new Map([['memory_brief', {}]]),
    ensureReady: () => ready ? Promise.resolve() : new Promise(resolve => { release = () => { ready = true; resolve(); }; }),
    dispose: async () => {},
    callTool: async (...args) => { calls.push(args); return { structuredContent: { roots: [] } }; },
    recall: async (...args) => { calls.push(args); return { structuredContent: { context: 'evidence' } }; },
  };
  const runtime = install(api, { env: envFor(OTHER), client, resolveEnvironment: () => envFor(BOUND), automaticRecallMs: 50 });
  t.after(() => runtime.dispose());
  const context = ctx(BOUND, { notices });
  let done = false;
  const pending = api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context)
    .then(result => { done = true; return result; });
  await new Promise(setImmediate);
  assert.equal(typeof release, 'function');
  t.mock.timers.tick(49);
  await new Promise(setImmediate);
  assert.equal(done, false);
  t.mock.timers.tick(1);
  assert.equal(await pending, undefined);
  assert.equal(notices.filter(n => /recall unavailable/.test(n.message)).length, 1);
  release();
  await new Promise(setImmediate);
  assert.deepEqual(calls, []);
  const fresh = await api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  assert.ok(fresh.message);
  assert.equal(calls.length, 2);
});

test('the recall deadline is armed before a slow asynchronous environment resolver', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const notices = [];
  let resolveEnv;
  const calls = [];
  const client = {
    tools: new Map([['memory_brief', {}]]),
    ensureReady: () => Promise.resolve(),
    dispose: async () => {},
    callTool: async (...args) => { calls.push(args); return { structuredContent: { roots: [] } }; },
    recall: async (...args) => { calls.push(args); return { structuredContent: { context: 'evidence' } }; },
  };
  const runtime = install(api, {
    env: envFor(OTHER), client, automaticRecallMs: 50,
    resolveEnvironment: () => new Promise(resolve => { resolveEnv = resolve; }),
  });
  t.after(() => runtime.dispose());
  const context = ctx(BOUND, { notices });
  const pending = api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  await new Promise(setImmediate);
  assert.equal(typeof resolveEnv, 'function');
  t.mock.timers.tick(50);
  assert.equal(await pending, undefined);
  assert.equal(notices.filter(n => /recall unavailable/.test(n.message)).length, 1);
  resolveEnv(envFor(BOUND));
  await new Promise(setImmediate);
  assert.deepEqual(calls, []);
});

test('one deadline covers binding and subsequent recall', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const notices = [];
  let release;
  let ready = false;
  let recalled = false;
  const client = {
    tools: new Map(),
    ensureReady: () => ready ? Promise.resolve() : new Promise(resolve => { release = () => { ready = true; resolve(); }; }),
    dispose: async () => {},
    recall: () => { recalled = true; return new Promise(() => {}); },
  };
  const runtime = install(api, { env: envFor(OTHER), client, resolveEnvironment: () => envFor(BOUND), automaticRecallMs: 50 });
  t.after(() => runtime.dispose());
  let done = false;
  const pending = api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, ctx(BOUND, { notices }))
    .then(result => { done = true; return result; });
  await new Promise(setImmediate);
  t.mock.timers.tick(30);
  release();
  await new Promise(setImmediate);
  assert.equal(recalled, true);
  t.mock.timers.tick(19);
  await new Promise(setImmediate);
  assert.equal(done, false);
  t.mock.timers.tick(1);
  assert.equal(await pending, undefined);
  assert.equal(notices.filter(n => /recall unavailable/.test(n.message)).length, 1);
});

test('ignored reasons and non-string prompts start unbound binding without waiting', async () => {
  for (const event of [{ prompt: null }, { prompt: 'substantive project request please', reason: 'revive' }]) {
    const api = fakeApi();
    let release;
    const client = { ensureReady: () => new Promise(resolve => { release = resolve; }), dispose: async () => {} };
    const runtime = install(api, { env: envFor(OTHER), client, resolveEnvironment: () => envFor(BOUND) });
    let done = false;
    const pending = api.emit('before_agent_start', event, ctx(BOUND)).then(result => { done = true; return result; });
    await new Promise(setImmediate);
    assert.equal(done, true);
    assert.equal(await pending, undefined);
    assert.equal(typeof release, 'function');
    release();
    await runtime.dispose();
  }
});

test('unbound trivial prompt returns immediately while binding proceeds', async (t) => {
  const api = fakeApi();
  let release;
  const client = {
    ensureReady: () => new Promise(resolve => { release = resolve; }),
    dispose: async () => {},
  };
  const runtime = install(api, { env: envFor(OTHER), client, resolveEnvironment: () => envFor(BOUND) });
  t.after(() => runtime.dispose());
  let done = false;
  const pending = api.emit('before_agent_start', { prompt: 'ok' }, ctx(BOUND))
    .then(result => { done = true; return result; });
  await new Promise(setImmediate);
  assert.equal(done, true);
  assert.equal(await pending, undefined);
  assert.equal(typeof release, 'function');
  release();
});

test('an expired automatic hook cannot admit recall after a cold handshake finishes', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  const notices = [];
  let releaseInitialize;
  const spawn = () => {
    const child = fakeChild((message, process) => {
      if (message.method === 'initialize') {
        releaseInitialize = () => process.stdout.write(JSON.stringify(mcpHandler(state)(message)) + '\n');
        return null;
      }
      if (message.method === 'tools/list') {
        const result = mcpHandler(state)(message);
        result.result.tools = result.result.tools.filter(tool => tool.name !== 'memory_brief');
        return result;
      }
      return mcpHandler(state)(message);
    });
    child.stdin.once('finish', () => child.emit('exit', 0));
    return child;
  };
  const runtime = install(api, { env: envFor(BOUND), spawn, timeoutMs: 10000, automaticRecallMs: 50 });
  t.after(() => runtime.dispose());
  let completed = false;
  const expired = api.emit('before_agent_start', { prompt: 'expired cold prompt request' }, ctx(BOUND, { notices }))
    .then((result) => { completed = true; return result; });
  await new Promise(setImmediate);
  assert.equal(typeof releaseInitialize, 'function');
  t.mock.timers.tick(50);
  await new Promise(setImmediate);
  assert.equal(completed, true);
  assert.equal(await expired, undefined);
  assert.equal(notices[0].type, 'warning');
  releaseInitialize();
  await new Promise(setImmediate);
  assert.deepEqual(state.messages.filter((message) => message.method === 'tools/call'), []);
  const fresh = await api.emit('before_agent_start', { prompt: 'lembra fresh prompt project request' }, ctx(BOUND));
  assert.equal(fresh.message.content, formatRecall({ context: 'bound evidence' }).text);
});

test('one automatic deadline covers initialization and recall without overlapping or reusing late work', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  let releaseInitialize;
  let releaseRecall;
  let delayRecall = true;
  const spawn = () => {
    const child = fakeChild((message, process) => {
      if (message.method === 'initialize') {
        releaseInitialize = () => process.stdout.write(JSON.stringify(mcpHandler(state)(message)) + '\n');
        return null;
      }
      if (message.method === 'tools/call' && message.params?.name === 'memory_recall' && delayRecall) {
        state.messages.push(message);
        releaseRecall = () => process.stdout.write(JSON.stringify({
          jsonrpc: '2.0', id: message.id,
          result: { structuredContent: { context: 'late evidence for the expired prompt' } },
        }) + '\n');
        return null;
      }
      return mcpHandler(state)(message);
    });
    child.stdin.once('finish', () => child.emit('exit', 0));
    return child;
  };
  const runtime = install(api, { env: envFor(BOUND), spawn, timeoutMs: 1000, automaticRecallMs: 50 });
  t.after(() => runtime.dispose());
  let completed = false;
  const expired = api.emit('before_agent_start', { prompt: 'old prompt project request' }, ctx(BOUND))
    .then((result) => { completed = true; return result; });
  await new Promise(setImmediate);
  t.mock.timers.tick(30);
  releaseInitialize();
  await new Promise(setImmediate);
  assert.equal(typeof releaseRecall, 'function');
  assert.equal(completed, false);
  t.mock.timers.tick(20);
  await new Promise(setImmediate);
  assert.equal(completed, true);
  assert.equal(await expired, undefined);
  // Cross the actual NativeMcpClient request timeout as well as the hook deadline.
  t.mock.timers.tick(1000);
  await new Promise(setImmediate);
  assert.equal(runtime.client.pending.size, 1);
  assert.equal(await api.emit('before_agent_start', { prompt: 'lembra next prompt while busy' }, ctx(BOUND)), undefined);
  const admitted = state.messages.filter((message) => message.method === 'tools/call' && message.params?.name === 'memory_recall');
  assert.deepEqual(admitted.map((message) => message.params.arguments.task), ['old prompt project request']);
  releaseRecall();
  await new Promise(setImmediate);
  delayRecall = false;
  const fresh = await api.emit('before_agent_start', { prompt: 'lembra fresh prompt project request' }, ctx(BOUND));
  assert.equal(fresh.message.content, formatRecall({ context: 'bound evidence' }).text);
});


const turn = () => new Promise(setImmediate);

function reply(child, message, result) {
  child.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id: message.id, result })}\n`);
}

function receipt(source) {
  return { content: [{ type: 'text', text: JSON.stringify({ source, status: 'saved_pending' }) }], isError: false };
}

test('native timed-out requests retain bounded slots until backend settlement', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [] };
  let pending;
  const child = fakeChild((message) => {
    if (message.method === 'tools/call') { pending = message; return null; }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const client = new NativeMcpClient({ env: envFor(), spawn: () => child, timeoutMs: 40, maxPending: 1 });
  t.after(() => client.dispose());
  let settled = false;
  const call = client.recall({ task: 'bounded' }, { onPending: (lifetime) => lifetime.then(() => { settled = true; }) });
  const rejected = assert.rejects(call, /timeout/);
  await turn();
  t.mock.timers.tick(40);
  await rejected;
  assert.equal(settled, false);
  assert.equal(client.pending.size, 1);
  await assert.rejects(client.recall({ task: 'must not overlap' }), /pending bound/);
  reply(child, pending, { structuredContent: { context: 'late' } });
  await turn();
  assert.equal(settled, true);
  assert.equal(client.pending.size, 0);
  const fresh = client.recall({ task: 'fresh' });
  await turn();
  reply(child, pending, { structuredContent: { context: 'fresh context' } });
  assert.equal(extractRecallContext(await fresh), 'fresh context');
});

test('automatic lifetime waits for confirmed exit, not merely a kill attempt', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [] };
  const child = fakeChild((message) => message.method === 'tools/call' ? null : mcpHandler(state)(message));
  child.kill = () => true; // An attempted kill is not a backend settlement receipt.
  const client = new NativeMcpClient({ env: envFor(), spawn: () => child, timeoutMs: 20, shutdownMs: 10 });
  let settled = false;
  const call = client.recall({ task: 'slow' }, { onPending: (lifetime) => lifetime.then(() => { settled = true; }) });
  const rejected = assert.rejects(call, /timeout/);
  await turn();
  t.mock.timers.tick(20);
  await rejected;
  const disposal = client.dispose();
  t.mock.timers.tick(10);
  await disposal;
  assert.equal(settled, false);
  child.emit('exit', 0);
  await turn();
  assert.equal(settled, true);
});

test('explicit calls retain their ordinary request deadline', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [] };
  let pending;
  const child = fakeChild((message) => {
    if (message.method === 'tools/call') { pending = message; return null; }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const client = new NativeMcpClient({ env: envFor(), spawn: () => child, timeoutMs: 660000 });
  t.after(() => client.dispose());
  let completed = false;
  const call = client.capture({ source: 'explicit' }).then((result) => { completed = true; return result; });
  await turn();
  t.mock.timers.tick(25001);
  await turn();
  assert.equal(completed, false);
  reply(child, pending, receipt('explicit'));
  assert.equal((await call).isError, false);
});

test('capture queue warns on overflow and shutdown drains all accepted metadata once', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const notices = [];
  const context = ctx(BOUND, { notices });
  const state = { messages: [], captures: [] };
  let first;
  const child = fakeChild((message) => {
    if (message.method === 'tools/call' && message.params.name === 'memory_capture') {
      state.captures.push(message.params.arguments);
      if (!first) { first = message; return null; }
      return { jsonrpc: '2.0', id: message.id, result: receipt(message.params.arguments.source) };
    }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const runtime = install(api, { env: envFor(), spawn: () => child, captureQueueMax: 1000 });
  await api.emit('tool_result', toolEvent(), context);
  await turn();
  for (let i = 0; i < 33; i++) await api.emit('tool_result', toolEvent({ toolCallId: `queued-${i}` }), context);
  assert.equal(notices.length, 1);
  assert.match(notices[0].message, /queue full/);
  const shutdown = api.emit('session_shutdown', {}, context);
  reply(child, first, receipt(first.params.arguments.source));
  await shutdown;
  assert.equal(state.captures.length, 33);
  assert.equal(new Set(state.captures.map((item) => item.source)).size, 33);
  assert.ok(state.captures.every((item) => JSON.parse(item.content).tool === 'edit' && item.kind === 'action'));
  assert.ok(!JSON.stringify(notices).includes(SECRET));
});

test('binding changes drain the old client/project and discard late context and old-session events', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const state = { messages: [], captures: [] };
  const children = [];
  const captures = [];
  let pendingCapture;
  let pendingRecall;
  const projectEnv = (cwd) => ({ ...envFor(cwd), LAMBDA_MEMORY_PROJECT: cwd === BOUND ? 'old-project' : 'new-project' });
  const runtime = install(api, {
    env: projectEnv(BOUND),
    resolveEnvironment: (context) => projectEnv(context.cwd),
    spawn: (_bin, _argv, options) => {
      const project = options.env.LAMBDA_MEMORY_PROJECT;
      const child = fakeChild((message) => {
        if (message.method === 'tools/call') {
          if (message.params.name === 'memory_recall' && project === 'old-project') { pendingRecall = message; return null; }
          if (message.params.name === 'memory_capture') {
            captures.push({ project, payload: message.params.arguments });
            if (!pendingCapture) { pendingCapture = message; return null; }
            return { jsonrpc: '2.0', id: message.id, result: receipt(message.params.arguments.source) };
          }
        }
        return mcpHandler(state)(message);
      });
      child.stdin.once('finish', () => child.emit('exit', 0));
      children.push(child);
      return child;
    },
  });
  const oldCtx = ctx(BOUND);
  const newCtx = ctx(OTHER, { sessionId: 'new-session' });
  await api.emit('session_start', {}, oldCtx);
  const recall = api.emit('before_agent_start', { prompt: 'old task project request' }, oldCtx);
  await turn();
  await api.emit('tool_result', toolEvent(), oldCtx);
  await turn();
  await api.emit('tool_result', toolEvent({ toolCallId: 'old-queued' }), oldCtx);
  const switching = api.emit('session_switch', {}, newCtx);
  await api.emit('tool_result', toolEvent({ toolCallId: 'during-transition' }), newCtx);
  reply(children[0], pendingRecall, { structuredContent: { context: 'must never inject' } });
  reply(children[0], pendingCapture, receipt(pendingCapture.params.arguments.source));
  await switching;
  assert.equal(await recall, undefined);
  await api.emit('tool_result', toolEvent({ toolCallId: 'stale-old-session' }), oldCtx);
  await api.emit('tool_result', toolEvent({ toolCallId: 'new-project-event' }), newCtx);
  await runtime.flush();
  assert.deepEqual(captures.map(({ project, payload }) => [project, payload.project, payload.source]), [
    ['old-project', 'old-project', sourceId('session-1', 'call-1')],
    ['old-project', 'old-project', sourceId('session-1', 'old-queued')],
    ['new-project', 'new-project', sourceId('new-session', 'new-project-event')],
  ]);
  await runtime.dispose();
});

test('bounded shutdown reports abandoned captures without retrying indeterminate writes', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const notices = [];
  const context = ctx(BOUND, { notices });
  const state = { messages: [], captures: [] };
  let calls = 0;
  const child = fakeChild((message) => {
    if (message.method === 'tools/call') { calls += 1; return null; }
    return mcpHandler(state)(message);
  });
  const runtime = install(api, { env: envFor(), spawn: () => child, timeoutMs: 660000 });
  await api.emit('tool_result', toolEvent(), context);
  await turn();
  await api.emit('tool_result', toolEvent({ toolCallId: 'queued' }), context);
  let done = false;
  const shutdown = api.emit('session_shutdown', {}, context).then(() => { done = true; });
  t.mock.timers.tick(500);
  await turn();
  assert.ok(notices.some((notice) => /dropped/.test(notice.message)));
  t.mock.timers.tick(1010);
  await shutdown;
  assert.equal(done, true);
  assert.equal(calls, 1);
  assert.equal(child.killed, 'SIGKILL');
  assert.ok(notices.some((notice) => /not confirmed/.test(notice.message)));
  assert.ok(!JSON.stringify(notices).includes(SECRET));
});

test('automatic captures warn on RPC rejection, isError, malformed and mismatched receipts without retry', async (t) => {
  const cases = [
    { error: { code: -32602, message: SECRET } },
    { result: { isError: true, content: [{ type: 'text', text: SECRET }] } },
    { result: { content: [{ type: 'text', text: SECRET }] } },
    { result: receipt('wrong-source') },
  ];
  for (const envelope of cases) {
    await t.test(JSON.stringify(Object.keys(envelope)), async () => {
      const state = { messages: [], captures: [] };
      const api = fakeApi();
      const notices = [];
      let calls = 0;
      const child = fakeChild((message) => {
        if (message.method === 'tools/call') {
          calls += 1;
          return { jsonrpc: '2.0', id: message.id, ...envelope };
        }
        return mcpHandler(state)(message);
      });
      child.stdin.once('finish', () => child.emit('exit', 0));
      const runtime = install(api, { env: envFor(), spawn: () => child });
      await api.emit('tool_result', toolEvent(), ctx(BOUND, { notices }));
      await runtime.flush();
      assert.equal(calls, 1);
      assert.equal(notices.length, 1);
      assert.equal(notices[0].type, 'warning');
      assert.ok(!JSON.stringify(notices).includes(SECRET));
      await runtime.dispose();
    });
  }
});



test('explicit capture waiting for readiness cannot cross a session transition', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const state = { messages: [], captures: [] };
  const notices = [];
  let initialize;
  const child = fakeChild((message) => {
    if (message.method === 'initialize') { initialize = message; return null; }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const runtime = install(api, { env: envFor(), spawn: () => child });
  const command = api.commands.get('lambda-memory').handler(SECRET, ctx(BOUND, { notices }));
  await turn();
  const switched = api.emit('session_switch', {}, ctx(BOUND, { sessionId: 'new-session' }));
  child.stdout.write(`${JSON.stringify(mcpHandler(state)(initialize))}\n`);
  await Promise.all([command, switched]);
  assert.deepEqual(state.captures, []);
  assert.equal(notices.at(-1).type, 'warning');
  assert.ok(!JSON.stringify(notices).includes(SECRET));
  await runtime.dispose();
});

test('project transition stays bounded during stuck old capture and admits fresh project work', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const notices = [];
  const state = { messages: [], captures: [] };
  const captures = [];
  const children = [];
  const projectEnv = (cwd) => ({ ...envFor(cwd), LAMBDA_MEMORY_PROJECT: cwd === BOUND ? 'old-project' : 'new-project' });
  const runtime = install(api, {
    env: projectEnv(BOUND), timeoutMs: 660000,
    resolveEnvironment: (context) => projectEnv(context.cwd),
    spawn: (_bin, _argv, options) => {
      const project = options.env.LAMBDA_MEMORY_PROJECT;
      const child = fakeChild((message) => {
        if (message.method === 'tools/call' && message.params.name === 'memory_capture') {
          captures.push({ project, payload: message.params.arguments });
          if (project === 'old-project') return null;
          return { jsonrpc: '2.0', id: message.id, result: receipt(message.params.arguments.source) };
        }
        return mcpHandler(state)(message);
      });
      if (project === 'new-project') child.stdin.once('finish', () => child.emit('exit', 0));
      children.push(child);
      return child;
    },
  });
  await api.emit('tool_result', toolEvent(), ctx(BOUND, { notices }));
  await turn();
  await api.emit('tool_result', toolEvent({ toolCallId: 'dropped-old' }), ctx(BOUND, { notices }));
  const newCtx = ctx(OTHER, { sessionId: 'new-session' });
  const switched = api.emit('session_switch', {}, newCtx);
  await turn(); // the environment resolver is asynchronous
  t.mock.timers.tick(500);
  await turn();
  t.mock.timers.tick(1010);
  await switched;
  await api.emit('tool_result', toolEvent({ toolCallId: 'fresh' }), newCtx);
  await runtime.flush();
  assert.deepEqual(captures.map(({ project, payload }) => [project, payload.project, payload.source]), [
    ['old-project', 'old-project', sourceId('session-1', 'call-1')],
    ['new-project', 'new-project', sourceId('new-session', 'fresh')],
  ]);
  assert.equal(children[0].killed, 'SIGKILL');
  assert.ok(notices.some((notice) => /dropped/.test(notice.message)));
  await runtime.dispose();
});

test('extension shutdown tightens an already running provider-length handshake teardown', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const api = fakeApi();
  const child = fakeChild(() => null);
  const runtime = install(api, {
    env: { ...envFor(), LAMBDA_MEMORY_MODE: 'openrouter' },
    spawn: () => child, timeoutMs: 40,
  });
  const recall = api.emit('before_agent_start', { prompt: 'cold project request please' }, ctx(BOUND));
  await turn();
  t.mock.timers.tick(40);
  await turn(); // initialize timeout has begun its ordinary 630-second teardown
  const shutdown = api.emit('session_shutdown', {}, ctx(BOUND));
  await turn();
  t.mock.timers.tick(1010);
  await shutdown;
  assert.equal(child.killed, 'SIGKILL');
  assert.equal(await recall, undefined);
});

test('tool_result with pipe call id captures action episode', async () => {
  const state = { messages: [], captures: [], spawns: [], initialized: false };
  const api = fakeApi();
  const runtime = install(api, { env: envFor(BOUND), spawn: spawnRecorder(state, mcpHandler(state)) });
  await api.emit('tool_result', {
    toolName: 'bash',
    toolCallId: 'call_1|fc_2',
    isError: false,
    input: { command: 'cargo test --features x\nsecond line', i: 'Running tests' },
    content: [{ type: 'text', text: 'ok' }],
  }, ctx(BOUND, { sessionId: 'session-pipe' }));
  await runtime.flush();
  assert.equal(state.captures.length, 1);
  const [captured] = state.captures;
  assert.equal(captured.kind, 'action');
  assert.equal(captured.outcome, 'succeeded');
  assert.equal(captured.origin, 'omp://action');
  assert.match(captured.source, /^[0-9a-f]{16}\/[0-9a-f]{16}$/);
  const episode = JSON.parse(captured.content);
  assert.equal(episode.v, 1);
  assert.equal(episode.tool, 'bash');
  assert.equal(episode.summary, 'cargo test --features x');
  assert.equal(episode.intent, 'Running tests');
  assert.equal(episode.seq, 0);
  await runtime.dispose();
});

test('failed tool_result keeps the first error line and a per-session seq', async () => {
  const state = { messages: [], captures: [], spawns: [], initialized: false };
  const api = fakeApi();
  const runtime = install(api, { env: envFor(BOUND), spawn: spawnRecorder(state, mcpHandler(state)) });
  const first = ctx(BOUND, { sessionId: 'session-a' });
  await api.emit('tool_result', toolEvent({ toolName: 'bash', toolCallId: 'a1', isError: true, input: { command: 'cargo build' }, content: [{ type: 'text', text: '\nerror[E0599]: no method\nmore' }, { type: 'text', text: 'tail' }] }), first);
  await api.emit('tool_result', toolEvent({ toolName: 'bash', toolCallId: 'a2', input: { command: 'cargo build' }, content: [] }), first);
  await runtime.flush();
  const episodes = state.captures.map((item) => JSON.parse(item.content));
  assert.deepEqual(episodes.map((item) => item.seq), [0, 1]);
  assert.equal(episodes[0].error_head, 'error[E0599]: no method');
  assert.equal(state.captures[0].outcome, 'failed');
  assert.equal(episodes[1].error_head, null);
  await runtime.dispose();
});

test('successful read is not captured', async () => {
  const state = { messages: [], captures: [], spawns: [], initialized: false };
  const api = fakeApi();
  const runtime = install(api, { env: envFor(BOUND), spawn: spawnRecorder(state, mcpHandler(state)) });
  await api.emit('tool_result', toolEvent({ toolName: 'read', toolCallId: 'r1', input: { path: 'a.rs' } }), ctx(BOUND));
  await runtime.flush();
  assert.deepEqual(state.captures, []);
  await runtime.dispose();
});

test('first prompt injects brief and recall', async () => {
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  const brief = { roots: [{ root: 'Decisions', nodes: [{ title: 'Architecture' }] }] };
  const recall = { hits: [{ node: 'node-1', path: ['Decisions'], content: 'bound evidence' }] };
  const base = mcpHandler(state);
  const handler = (message) => {
    if (message.method === 'tools/list') {
      const result = base(message);
      return result;
    }
    if (message.method === 'tools/call' && ['memory_brief', 'memory_recall'].includes(message.params.name)) {
      state.messages.push(message);
      return { jsonrpc: '2.0', id: message.id, result: { structuredContent: message.params.name === 'memory_brief' ? brief : recall } };
    }
    return base(message);
  };
  const runtime = install(api, { env: envFor(), spawn: spawnRecorder(state, handler) });
  const context = ctx(BOUND);
  await api.emit('session_start', {}, context);
  const result = await api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  assert.equal(result.message.content, `${formatBrief(brief)}\n\n${formatRecall(recall).text}`);
  assert.deepEqual(state.messages.find(m => m.params?.name === 'memory_recall').params.arguments.exclude, []);
  assert.equal(state.messages.find(m => m.params?.name === 'memory_recall').params.arguments.budget_bytes, 6000);
  await runtime.dispose();
});

function gateFixture() {
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  const brief = { roots: [{ root: 'Decisions', nodes: [{ title: 'Architecture' }] }] };
  const hit = { hits: [{ node: 'node-1', path: ['Decisions'], content: 'bound evidence' }] };
  const base = mcpHandler(state);
  const handler = message => {
    if (message.method === 'tools/call' && ['memory_brief', 'memory_recall'].includes(message.params.name)) {
      state.messages.push(message);
      return { jsonrpc: '2.0', id: message.id, result: { structuredContent: message.params.name === 'memory_brief' ? brief : hit } };
    }
    return base(message);
  };
  const runtime = install(api, { env: envFor(), spawn: spawnRecorder(state, handler) });
  const context = ctx(BOUND);
  const calls = () => state.messages.filter(m => m.method === 'tools/call');
  const prompt = text => api.emit('before_agent_start', { prompt: text }, context);
  return { state, api, runtime, context, brief, hit, calls, prompt };
}

test('trivial prompt makes no memory call', async () => {
  const f = gateFixture();
  await f.prompt('Explain the project architecture please');
  const count = f.calls().length;
  assert.equal(await f.prompt('ok'), undefined);
  assert.equal(f.calls().length, count);
  await f.runtime.dispose();
});

test('a sanitized trivial prompt leaves the brief due', async () => {
  const f = gateFixture();
  assert.equal(await f.prompt(`${BOUND}/x`), undefined);
  assert.equal(f.calls().length, 0);
  assert.ok((await f.prompt('Explain the project architecture please')).message);
  await f.runtime.dispose();
});

test('related prompt does not recall again', async () => {
  const f = gateFixture();
  await f.prompt('Explain the project architecture please');
  assert.equal(await f.prompt('Explain the project architecture further'), undefined);
  assert.equal(f.calls().filter(m => m.params.name === 'memory_recall').length, 1);
  await f.runtime.dispose();
});

test('cue prompt recalls with exclusion', async () => {
  const f = gateFixture();
  await f.prompt('Explain the project architecture please');
  await f.prompt('lembra project architecture decisions');
  const calls = f.calls().filter(m => m.params.name === 'memory_recall');
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1].params.arguments.exclude, ['node-1']);
  await f.runtime.dispose();
});

test('compaction re-arms brief and recall', async () => {
  const f = gateFixture();
  await f.prompt('Explain the project architecture please');
  await f.api.emit('session_compact', {}, f.context);
  const result = await f.prompt('Explain the project architecture further');
  assert.equal(result.message.content, `${formatBrief(f.brief)}\n\n${formatRecall(f.hit).text}`);
  assert.equal(f.calls().filter(m => m.params.name === 'memory_brief').length, 2);
  assert.equal(f.calls().filter(m => m.params.name === 'memory_recall').length, 2);
  await f.runtime.dispose();
});

test('recall deadline is RECALL_DEADLINE_MS', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [] };
  const api = fakeApi();
  const notices = [];
  const child = fakeChild(message => {
    if (message.method === 'tools/call' && message.params.name === 'memory_recall') {
      state.messages.push(message);
      return null;
    }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const runtime = install(api, { env: envFor(), spawn: () => child, timeoutMs: 10000 });
  t.after(() => runtime.dispose());
  await api.emit('session_start', {}, ctx(BOUND, { notices }));
  let completed = false;
  const pending = api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, ctx(BOUND, { notices }))
    .then(result => { completed = true; return result; });
  await turn();
  t.mock.timers.tick(3499);
  await turn();
  assert.equal(completed, false);
  t.mock.timers.tick(1);
  assert.equal(await pending, undefined);
  assert.equal(notices.filter(n => /recall unavailable/.test(n.message)).length, 1);
});

test('recall deadline returns an answered brief only and does not repeat it', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [] };
  const api = fakeApi();
  const brief = { roots: [{ root: 'Decisions', nodes: [{ title: 'Architecture' }] }] };
  let pendingRecall;
  const child = fakeChild(message => {
    if (message.method === 'tools/call' && message.params.name === 'memory_brief') {
      state.messages.push(message);
      return { jsonrpc: '2.0', id: message.id, result: { structuredContent: brief } };
    }
    if (message.method === 'tools/call' && message.params.name === 'memory_recall') {
      state.messages.push(message);
      pendingRecall = message;
      return null;
    }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const runtime = install(api, { env: envFor(), spawn: () => child, timeoutMs: 10000, automaticRecallMs: 50 });
  t.after(() => runtime.dispose());
  const context = ctx(BOUND);
  const pending = api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  await turn();
  assert.ok(pendingRecall);
  t.mock.timers.tick(50);
  const result = await pending;
  assert.equal(result.message.content, formatBrief(brief));
  assert.equal(result.message.customType, 'lambda-memory-recall');
  assert.equal(await api.emit('before_agent_start', { prompt: 'Explain the project architecture further' }, context), undefined);
  assert.equal(state.messages.filter(m => m.params?.name === 'memory_brief').length, 1);
  assert.equal(state.messages.filter(m => m.params?.name === 'memory_recall').length, 1);
  reply(child, pendingRecall, { structuredContent: { context: 'late recall' } });
});

test('a failed brief is not retried when recall misses the deadline', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [] };
  const api = fakeApi();
  let briefCalls = 0;
  const child = fakeChild(message => {
    if (message.method === 'tools/call' && message.params.name === 'memory_brief') {
      briefCalls++;
      return { jsonrpc: '2.0', id: message.id, error: { code: -32000, message: 'down' } };
    }
    if (message.method === 'tools/call' && message.params.name === 'memory_recall') return null;
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const runtime = install(api, { env: envFor(), spawn: () => child, timeoutMs: 10000, automaticRecallMs: 50 });
  t.after(() => runtime.dispose());
  const context = ctx(BOUND);
  const pending = api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  await turn();
  t.mock.timers.tick(50);
  assert.equal(await pending, undefined);
  assert.equal(await api.emit('before_agent_start', { prompt: 'Explain the project architecture further' }, context), undefined);
  assert.equal(briefCalls, 1);
});

test('a brief sent before the outer deadline is not retried while an answered recall is kept', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [] };
  const api = fakeApi();
  const brief = { roots: [{ root: 'Decisions', nodes: [{ title: 'Architecture' }] }] };
  let pendingBrief;
  let briefCalls = 0;
  const child = fakeChild(message => {
    if (message.method === 'tools/call' && message.params.name === 'memory_brief') {
      state.messages.push(message);
      if (++briefCalls === 1) { pendingBrief = message; return null; }
      return { jsonrpc: '2.0', id: message.id, result: { structuredContent: brief } };
    }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const runtime = install(api, { env: envFor(), spawn: () => child, timeoutMs: 10000, automaticRecallMs: 50 });
  t.after(() => runtime.dispose());
  const context = ctx(BOUND);
  const pending = api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  await turn();
  assert.ok(pendingBrief);
  t.mock.timers.tick(50);
  const first = await pending;
  assert.ok(first.message.content.startsWith('Lambda Memory recall'));
  assert.ok(!first.message.content.includes(formatBrief(brief)));
  reply(child, pendingBrief, { structuredContent: brief });
  await turn();
  const result = await api.emit('before_agent_start', { prompt: 'Explain the project architecture further' }, context);
  assert.equal(result, undefined);
  assert.equal(briefCalls, 1);
});

test('a brief that misses its own deadline is not retried', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = { messages: [], captures: [] };
  const api = fakeApi();
  const brief = { roots: [{ root: 'Decisions', nodes: [{ title: 'Architecture' }] }] };
  let briefCalls = 0;
  const child = fakeChild(message => {
    if (message.method === 'tools/call' && message.params.name === 'memory_brief') {
      if (++briefCalls === 1) return null;
      return { jsonrpc: '2.0', id: message.id, result: { structuredContent: brief } };
    }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const runtime = install(api, { env: envFor(), spawn: () => child, timeoutMs: 10000 });
  t.after(() => runtime.dispose());
  const context = ctx(BOUND);
  const pending = api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  await turn();
  t.mock.timers.tick(2000);
  const first = await pending;
  assert.ok(!first?.message?.content.includes('Lambda Memory brief'));
  const result = await api.emit('before_agent_start', { prompt: 'Explain the project architecture further' }, context);
  assert.ok(!result?.message?.content.includes('Lambda Memory brief'));
  assert.equal(briefCalls, 1);
});

test('reason other than prompt is ignored', async () => {
  const f = gateFixture();
  assert.equal(await f.api.emit('before_agent_start', { prompt: 'Explain the project architecture please', reason: 'revive' }, f.context), undefined);
  assert.equal(f.calls().length, 0);
  assert.ok((await f.prompt('Explain the project architecture please')).message);
  await f.runtime.dispose();
});

test('subagent gets no brief or recall and cannot re-arm the main session', async () => {
  const f = gateFixture();
  const sub = ctx(BOUND, { agentKind: 'sub', sessionId: 'sub-1' });
  assert.equal(await f.api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, sub), undefined);
  await f.api.emit('session_compact', {}, sub);
  assert.equal(f.calls().length, 0);
  assert.ok((await f.prompt('Explain the project architecture please')).message);
  await f.api.emit('session_compact', {}, sub);
  assert.equal(await f.prompt('Explain the project architecture further'), undefined);
  await f.runtime.dispose();
});

test('empty and non-prompt events leave the brief and gate untouched', async () => {
  const f = gateFixture();
  for (const event of [{ prompt: '' }, { prompt: null }, { prompt: '   ' }, { prompt: 'A substantive request about architecture', reason: 'revive' }]) {
    assert.equal(await f.api.emit('before_agent_start', event, f.context), undefined);
  }
  assert.equal(f.calls().length, 0);
  assert.ok((await f.prompt('Explain the project architecture please')).message);
  assert.equal(f.calls().length, 2);
  await f.runtime.dispose();
});

test('missing brief tool is skipped silently', async () => {
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  const notices = [];
  const base = mcpHandler(state);
  const handler = message => {
    const response = base(message);
    if (message.method === 'tools/list') response.result.tools = response.result.tools.filter(t => t.name !== 'memory_brief');
    return response;
  };
  const runtime = install(api, { env: envFor(), spawn: spawnRecorder(state, handler) });
  const result = await api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, ctx(BOUND, { notices }));
  assert.ok(result.message);
  assert.equal(state.messages.filter(m => m.params?.name === 'memory_brief').length, 0);
  assert.deepEqual(notices, []);
  await runtime.dispose();
});

test('brief failure is silent and not retried until compaction', async () => {
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  const notices = [];
  const base = mcpHandler(state);
  const handler = message => {
    if (message.method === 'tools/call' && message.params.name === 'memory_brief') {
      state.messages.push(message);
      return { jsonrpc: '2.0', id: message.id, error: { code: -32000, message: 'down' } };
    }
    return base(message);
  };
  const runtime = install(api, { env: envFor(), spawn: spawnRecorder(state, handler) });
  const context = ctx(BOUND, { notices });
  await api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  await api.emit('before_agent_start', { prompt: 'Explain the project architecture further' }, context);
  assert.equal(state.messages.filter(m => m.params?.name === 'memory_brief').length, 1);
  assert.deepEqual(notices, []);
  await api.emit('session_compact', {}, context);
  await api.emit('before_agent_start', { prompt: 'Explain the project architecture further' }, context);
  assert.equal(state.messages.filter(m => m.params?.name === 'memory_brief').length, 2);
  await runtime.dispose();
});

test('brief is written before recall on the shared connection and recall is sanitized', async () => {
  const state = { messages: [], captures: [] };
  const api = fakeApi();
  let pendingBrief;
  const child = fakeChild(message => {
    if (message.method === 'tools/call' && message.params.name === 'memory_brief') {
      pendingBrief = message;
      return null;
    }
    return mcpHandler(state)(message);
  });
  child.stdin.once('finish', () => child.emit('exit', 0));
  const runtime = install(api, { env: envFor(), spawn: () => child });
  const pending = api.emit('before_agent_start', { prompt: `Explain project architecture ${BOUND}/private.txt` }, ctx(BOUND));
  await turn();
  const recall = state.messages.find(m => m.params?.name === 'memory_recall');
  assert.ok(recall);
  assert.ok(!recall.params.arguments.task.includes(BOUND));
  assert.ok(pendingBrief);
  assert.ok(pendingBrief.id < recall.id, 'the brief request is written first');
  reply(child, pendingBrief, { structuredContent: { roots: [] } });
  assert.ok((await pending).message);
  await runtime.dispose();
});

test('session switch retains gate state and oldest sessions are evicted', async () => {
  const f = gateFixture();
  await f.prompt('Explain the project architecture please');
  const other = ctx(BOUND, { sessionId: 'other-session' });
  await f.api.emit('session_switch', {}, other);
  await f.api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, other);
  await f.api.emit('session_switch', {}, f.context);
  assert.equal(await f.prompt('Explain the project architecture further'), undefined);
  for (let i = 0; i < 31; i++) {
    const context = ctx(BOUND, { sessionId: `session-${i + 2}` });
    await f.api.emit('session_switch', {}, context);
    await f.api.emit('before_agent_start', { prompt: 'Explain the project architecture please' }, context);
  }
  await f.api.emit('session_switch', {}, f.context);
  assert.ok((await f.prompt('Explain the project architecture further')).message);
  await f.runtime.dispose();
});

function exchangeFixture(options = {}) {
  const state = { messages: [], captures: [], spawns: [] };
  const api = fakeApi();
  const extras = { branch: [], notices: [], ...options.extras };
  const context = ctx(BOUND, extras);
  const runtime = install(api, {
    env: envFor(), captureConversation: true,
    spawn: spawnRecorder(state, options.handler?.(state) ?? mcpHandler(state)),
    ...options.install,
  });
  return { state, api, extras, context, runtime };
}

const userEntry = (id, content) => ({ type: 'message', id, message: { role: 'user', content } });
const assistantEnd = (content = [{ type: 'text', text: 'R' }], extra = {}) => ({
  type: 'agent_end', messages: [{ role: 'assistant', content }], ...extra,
});

test('main-agent turn captures one exchange', async () => {
  const { state, api, extras, context, runtime } = exchangeFixture();
  extras.branch.push(userEntry('e1', 'P'));
  await api.emit('session_start', {}, context);
  await api.emit('agent_end', assistantEnd(), context);
  await runtime.flush();
  assert.deepEqual(state.captures, [{ project: 'lambda-memory', source: exchangeSource('session-1', 'omp-entry-e1', 'P'),
    kind: 'exchange', outcome: 'unknown', origin: 'omp://exchange', content: exchangeContent('P', 'R') }]);
  await runtime.dispose();
});

test('array content and thinking parts', async () => {
  const { state, api, extras, context, runtime } = exchangeFixture();
  extras.branch.push(userEntry('old', 'old'), userEntry('e1', [{ type: 'text', text: 'a' }, { type: 'image' }, { type: 'text', text: 'b' }]));
  await api.emit('session_start', {}, context);
  await api.emit('agent_end', assistantEnd([{ type: 'thinking', thinking: 'private' }, { type: 'toolCall', text: 'ignore' }, { type: 'text', text: 'reply' }]), context);
  await runtime.flush();
  assert.equal(state.captures[0].content, exchangeContent('a\nb', 'reply'));
  await runtime.dispose();
});

for (const [name, installOptions, contextExtras, eventExtra, cwd] of [
  ['willContinue true captures nothing', {}, {}, { willContinue: true }, BOUND],
  ['subagent captures nothing', {}, { agentKind: 'sub' }, {}, BOUND],
  ['captureConversation false captures nothing', { captureConversation: false }, {}, {}, BOUND],
  ['unbound session captures nothing', {}, {}, {}, OTHER],
  ['empty session id captures nothing', {}, { sessionId: '' }, {}, BOUND],
]) {
  test(name, async () => {
    const fixture = exchangeFixture({ install: installOptions, extras: contextExtras });
    fixture.extras.branch.push(userEntry('e1', 'P'));
    await fixture.api.emit('agent_end', assistantEnd(undefined, eventExtra), ctx(cwd, fixture.extras));
    await fixture.runtime.flush();
    assert.deepEqual(fixture.state.captures, []);
    await fixture.runtime.dispose();
  });
}

test('same user entry twice captures once', async () => {
  const { state, api, extras, context, runtime } = exchangeFixture();
  extras.branch.push(userEntry('e1', 'P'));
  await api.emit('session_start', {}, context);
  await api.emit('agent_end', assistantEnd(), context);
  await api.emit('agent_end', assistantEnd([{ type: 'text', text: 'different' }]), context);
  await runtime.flush();
  assert.equal(state.captures.length, 1);
  await runtime.dispose();
});

test('no user entry falls back to the prompt hash', async () => {
  const { state, api, context, runtime } = exchangeFixture();
  await api.emit('session_start', {}, context);
  await api.emit('agent_end', assistantEnd(), context);
  await runtime.flush();
  assert.equal(state.captures[0].source, exchangeSource('session-1', null, ''));
  await runtime.dispose();
});

test('a Grafana key in the prompt is redacted', async () => {
  const { state, api, extras, context, runtime } = exchangeFixture();
  extras.branch.push(userEntry('e1', 'key glsa_' + 'A'.repeat(32) + '_1a2b3c4d'));
  await api.emit('session_start', {}, context);
  await api.emit('agent_end', assistantEnd(), context);
  await runtime.flush();
  assert.equal(state.captures.length, 1);
  assert.ok(!state.captures[0].content.includes('glsa_A'));
  await runtime.dispose();
});

test('a rejected capture warns once per session', async () => {
  const fixture = exchangeFixture({ handler: (state) => (message) => {
    if (message.method === 'tools/call' && message.params?.name === 'memory_capture') {
      state.captures.push(message.params.arguments);
      return { jsonrpc: '2.0', id: message.id, error: { code: -32602, message: 'rejected' } };
    }
    return mcpHandler(state)(message);
  } });
  const { state, api, extras, context, runtime } = fixture;
  await api.emit('session_start', {}, context);
  for (const id of ['e1', 'e2']) {
    extras.branch.push(userEntry(id, 'P'));
    await api.emit('agent_end', assistantEnd(), context);
  }
  await runtime.flush();
  assert.equal(state.captures.length, 2);
  assert.equal(extras.notices.filter((notice) => /capture/.test(notice.message)).length, 1);
  await runtime.dispose();
});

test('null environment disables silently', async () => {
  const { state, api, extras, context, runtime } = exchangeFixture({ install: { resolveEnvironment: () => null } });
  extras.branch.push(userEntry('e1', 'P'));
  await api.emit('session_start', {}, context);
  await api.emit('before_agent_start', { prompt: 'task' }, context);
  await api.emit('before_agent_start', { prompt: 'x'.repeat(5000) }, context);
  await api.emit('agent_end', assistantEnd(), context);
  await api.emit('tool_result', toolEvent(), context);
  await runtime.flush();
  assert.deepEqual(state.spawns, []);
  assert.deepEqual(state.captures, []);
  assert.deepEqual(extras.notices, []);
  await runtime.dispose();
});


test('exchange deduplication is bounded and resets on session switch', async () => {
  const captures = [];
  const client = { closed: false, ensureReady: async () => {}, dispose: async () => {},
    capture: async (payload) => { captures.push(payload); return receipt(payload.source); } };
  const { api, extras, context, runtime } = exchangeFixture({ install: { client } });
  for (let i = 0; i < 257; i++) {
    extras.branch = [userEntry(`e${i}`, 'P')];
    await api.emit('agent_end', assistantEnd(), context);
  }
  await runtime.flush();
  extras.branch = [userEntry('e0', 'P')];
  await api.emit('agent_end', assistantEnd(), context);
  await runtime.flush();
  assert.equal(captures.length, 258);
  await api.emit('session_switch', {}, context);
  await api.emit('agent_end', assistantEnd(), context);
  await runtime.flush();
  assert.equal(captures.length, 259);
  await runtime.dispose();
});

test('exchange warning resets on session switch without changing action warnings', async () => {
  const notices = [];
  const client = { closed: false, ensureReady: async () => {}, dispose: async () => {},
    capture: async () => { throw new Error('rejected'); } };
  const { api, extras, context, runtime } = exchangeFixture({ install: { client }, extras: { notices } });
  extras.branch.push(userEntry('e1', 'P'));
  await api.emit('agent_end', assistantEnd(), context);
  await runtime.flush();
  await api.emit('tool_result', toolEvent(), context);
  await api.emit('tool_result', toolEvent({ toolCallId: 'second' }), context);
  await runtime.flush();
  assert.equal(notices.length, 3);
  await api.emit('session_switch', {}, context);
  await api.emit('agent_end', assistantEnd(), context);
  await runtime.flush();
  assert.equal(notices.length, 4);
  await runtime.dispose();
});

test('agent_end tolerates malformed events and throwing branch access', async () => {
  const { api, context, runtime } = exchangeFixture();
  context.sessionManager.getBranch = () => { throw new Error('bad branch'); };
  await api.emit('agent_end', assistantEnd(), context);
  await runtime.dispose();
});

test('null environment silently disables an oversized initial prompt', async () => {
  const { state, api, extras, runtime } = exchangeFixture({ install: {
    env: {}, resolveEnvironment: () => null,
  } });
  await api.emit('before_agent_start', { prompt: 'x'.repeat(5000) }, ctx(BOUND, extras));
  await new Promise(setImmediate);
  assert.deepEqual(extras.notices, []);
  assert.deepEqual(state.spawns, []);
  await runtime.dispose();
});

test('tool guard allows the user store identity', async () => {
  const api = fakeApi();
  const runtime = install(api, { env: envFor(), client: { closed: false, ensureReady: async () => {}, dispose: async () => {} } });
  await api.emit('session_start', {}, ctx(BOUND));
  const context = ctx(BOUND);
  assert.equal(await api.emit('tool_call', { toolName: 'mcp__lambda_memory_memory_node', input: { project: 'user:owner', node: 'n1' } }, context), undefined);
  const blocked = await api.emit('tool_call', { toolName: 'mcp__lambda_memory_memory_node', input: { project: 'user:other', node: 'n1' } }, context);
  assert.equal(blocked.block, true);
  await runtime.dispose();
});

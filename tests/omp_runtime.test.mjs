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
            { name: 'memory_recall', inputSchema: { type: 'object', properties: { task: { type: 'string' }, projects: { type: 'array', items: { type: 'string' } }, budget_tokens: { type: 'integer' } }, additionalProperties: false } },
            { name: 'memory_capture', inputSchema: { type: 'object', properties: { project: { type: 'string' }, source: { type: 'string' }, kind: { type: 'string' }, outcome: { type: 'string' }, origin: { type: 'string' }, content: { type: 'string' } }, additionalProperties: false } },
          ],
        },
      };
    }
    if (message.method === 'tools/call' && message.params?.name === 'memory_recall') {
      return { jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: JSON.stringify({ context: 'bound evidence' }) }], structuredContent: { context: 'bound evidence', mode: 'lexical', hits: [] }, isError: false } };
    }
    if (message.method === 'tools/call' && message.params?.name === 'memory_capture') {
      state.captures.push(message.params.arguments);
      return { jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: '{"status":"saved_pending"}' }], isError: false } };
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
    sessionManager: { getSessionId: () => extras.sessionId ?? 'session-1' },
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
  await api.emit('tool_result', toolEvent(), ctx(BOUND));
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
  assert.equal(state.captures[0].kind, 'error');
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
  const injected = await api.emit('before_agent_start', { prompt: 'what next' }, ctx(BOUND, { notices }));
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
  const first = await api.emit('before_agent_start', { prompt: 'task one' }, ctx(BOUND, { notices, sessionId: 's-malformed' }));
  const second = await api.emit('before_agent_start', { prompt: 'task two' }, ctx(BOUND, { notices, sessionId: 's-oversize' }));
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
  const recall = api.emit('before_agent_start', { prompt: 'old session' }, ctx(BOUND));
  for (let tries = 0; !deferred && tries < 100; tries++) await new Promise((resolve) => setTimeout(resolve, 1));
  await api.emit('session_switch', {}, ctx(BOUND, { sessionId: 'new-session' }));
  deferred();
  assert.equal(await recall, undefined);
  delay = false;
  await api.emit('session_shutdown', {}, ctx(BOUND, { agent: { kind: 'sub' } }));
  const fresh = await api.emit('before_agent_start', { prompt: 'new session' }, ctx(BOUND, { sessionId: 'new-session' }));
  assert.equal(fresh.message.content, 'bound evidence');
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
      return mcpHandler(state)(message);
    });
    child.stdin.once('finish', () => child.emit('exit', 0));
    return child;
  };
  const runtime = install(api, { env: envFor(BOUND), spawn, timeoutMs: 1000, automaticRecallMs: 50 });
  t.after(() => runtime.dispose());
  let completed = false;
  const expired = api.emit('before_agent_start', { prompt: 'expired cold prompt' }, ctx(BOUND, { notices }))
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
  const fresh = await api.emit('before_agent_start', { prompt: 'fresh prompt' }, ctx(BOUND));
  assert.equal(fresh.message.content, 'bound evidence');
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
  const expired = api.emit('before_agent_start', { prompt: 'old prompt' }, ctx(BOUND))
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
  assert.equal(await api.emit('before_agent_start', { prompt: 'next prompt while busy' }, ctx(BOUND)), undefined);
  const admitted = state.messages.filter((message) => message.method === 'tools/call' && message.params?.name === 'memory_recall');
  assert.deepEqual(admitted.map((message) => message.params.arguments.task), ['old prompt']);
  releaseRecall();
  await new Promise(setImmediate);
  delayRecall = false;
  const fresh = await api.emit('before_agent_start', { prompt: 'fresh prompt' }, ctx(BOUND));
  assert.equal(fresh.message.content, 'bound evidence');
});

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { runHook, UNAVAILABLE_CONTEXT } from '../runtime/agent-hook.mjs';
import { exchangeSource } from '../runtime/exchange.mjs';
import { loadConfig } from '../runtime/project-binding.mjs';
import { shortHash, sourceId } from '../runtime/action-episode.mjs';
import { emptyState, formatBrief, formatRecall, loadState, statePath } from '../runtime/recall-gate.mjs';

function fixture(t, { brief, recall, captureConversation, extraTools = '', ignoreRoots = [] } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'lm-agent-hook-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const project = join(root, 'project');
  const runtime = join(root, 'run');
  mkdirSync(project); mkdirSync(runtime);
  const log = join(root, 'calls.log');
  const bridge = join(root, 'bridge.sh');
  const exports = [['BRIEF', brief], ['RECALL', recall]].filter(([, value]) => value !== undefined)
    .map(([name, value]) => `export FAKE_NATIVE_${name}='${JSON.stringify(value)}'\n`).join('');
  writeFileSync(bridge, `#!/bin/sh\nexport FAKE_NATIVE_LOG="${log}"\n${exports}export FAKE_NATIVE_EXTRA_TOOLS=${JSON.stringify(extraTools)}\nexec "${process.execPath}" "${resolve('tests/support/fake-native.cjs')}" "$@"\n`, { mode: 0o755 });
  const config = join(root, 'runtime.json');
  writeFileSync(config, JSON.stringify({ nativeBin: bridge, bridgeBin: bridge, dataDir: join(root, 'data'), socketPath: join(runtime, 's.sock'), owner: 'owner', actor: 'actor', token: 'lambda-memory-hook-test-token-32b!!', ignoreRoots, ...(captureConversation === undefined ? {} : { captureConversation }) }), { mode: 0o600 });
  return { root, project, runtime, log, config };
}

function execute(script, args, input, f, env = {}) {
  const child = spawn(process.execPath, [resolve(script), ...args], { env: { PATH: process.env.PATH, HOME: f.root, LAMBDA_MEMORY_CONFIG: f.config, XDG_RUNTIME_DIR: f.runtime, ...env }, stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stdin.end(JSON.stringify(input));
  return new Promise(done => child.once('close', code => done({ code, stdout })));
}
const payload = f => ({ hook_event_name: 'PostToolUse', session_id: 'session', tool_use_id: 'call', cwd: f.project, tool_name: 'Bash', tool_input: { command: 'npm test', description: 'test' }, tool_response: { stdout: 'ok' } });
const captures = f => existsSync(f.log) ? readFileSync(f.log, 'utf8').split('\n').filter(Boolean).map(JSON.parse).filter(c => c.name === 'memory_capture') : [];

test('session-end calls memory_session_end', async t => {
  const f = fixture(t, { extraTools: 'memory_session_end' });
  const nested = join(f.project, 'nested'); mkdirSync(nested);
  const raw = 'raw harness session / ç';
  const result = await execute('runtime/agent-hook.mjs', ['claude-code', 'session-end'], {
    hook_event_name: 'SessionEnd', session_id: raw, cwd: nested,
  }, f, { CLAUDE_PROJECT_DIR: f.project });
  assert.deepEqual(result, { code: 0, stdout: '' });
  const calls = readFileSync(f.log, 'utf8').trim().split('\n').map(JSON.parse);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].name, 'memory_session_end');
  assert.deepEqual(calls[0].arguments, { project: `path:${f.project}`, session: raw });
  assert.equal(calls[0].project, `path:${f.project}`);
});

test('session-end ignores subagents', async t => {
  const f = fixture(t, { extraTools: 'memory_session_end' });
  for (const agent_id of ['sub', null]) {
    assert.deepEqual(await execute('runtime/agent-hook.mjs', ['claude-code', 'session-end'], {
      hook_event_name: 'SessionEnd', session_id: 'raw', cwd: f.project, agent_id,
    }, f), { code: 0, stdout: '' });
  }
  assert.equal(existsSync(f.log), false);
});

test('session-end ignores unbound directories and unavailable tools', async t => {
  const f = fixture(t, { ignoreRoots: [] });
  assert.deepEqual(await execute('runtime/agent-hook.mjs', ['claude-code', 'session-end'], {
    hook_event_name: 'SessionEnd', session_id: 'raw', cwd: f.project,
  }, f), { code: 0, stdout: '' });
  assert.equal(existsSync(f.log), false);
  const ignored = fixture(t, { extraTools: 'memory_session_end', ignoreRoots: [f.project] });
  assert.deepEqual(await execute('runtime/agent-hook.mjs', ['claude-code', 'session-end'], {
    hook_event_name: 'SessionEnd', session_id: 'raw', cwd: f.project,
  }, ignored), { code: 0, stdout: '' });
  assert.equal(existsSync(ignored.log), false);
});

test('session idle option is allowed in runtimeEnv', t => {
  const f = fixture(t);
  const config = JSON.parse(readFileSync(f.config, 'utf8'));
  config.runtimeEnv = { LAMBDA_MEMORY_SESSION_IDLE_MS: '60000' };
  writeFileSync(f.config, JSON.stringify(config));
  assert.equal(loadConfig({ LAMBDA_MEMORY_CONFIG: f.config }).runtimeEnv.LAMBDA_MEMORY_SESSION_IDLE_MS, '60000');
});

test('unknown harness prints nothing', async t => {
  const f = fixture(t);
  assert.equal(await runHook('unknown', 'post-tool-use', async () => payload(f)), '');
  assert.equal(captures(f).length, 0);
});

test('claude-code via agent-hook matches the shim', async t => {
  const f = fixture(t);
  const runExport = async (module, args) => {
    const previous = { ...process.env };
    Object.assign(process.env, { HOME: f.root, LAMBDA_MEMORY_CONFIG: f.config, XDG_RUNTIME_DIR: f.runtime });
    try { return await module.runHook(...args, async () => payload(f)); }
    finally { process.env = previous; }
  };
  const core = await import('../runtime/agent-hook.mjs');
  const shim = await import('../runtime/claude-code-hook.mjs');
  const first = await runExport(core, ['claude-code', 'post-tool-use']);
  rmSync(f.runtime, { recursive: true }); mkdirSync(f.runtime);
  const second = await runExport(shim, ['post-tool-use']);
  assert.equal(first, '');
  assert.equal(second, first);
  const calls = captures(f);
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[0].arguments, calls[1].arguments);
});

test('relative cwd records nothing', async t => {
  const f = fixture(t);
  const result = await execute('runtime/agent-hook.mjs', ['claude-code', 'post-tool-use'], { ...payload(f), cwd: 'tmp' }, f);
  assert.deepEqual(result, { code: 0, stdout: '' });
  assert.equal(captures(f).length, 0);
});

const codexPayload = (f, extra = {}) => ({
  session_id: 'session', turn_id: 'turn', cwd: f.project, hook_event_name: 'PostToolUse',
  model: 'model', permission_mode: 'default', tool_name: 'Bash', tool_use_id: 'id',
  tool_input: { command: 'cargo test\nmore' }, tool_response: 'output', transcript_path: null, ...extra,
});
const codexRun = (f, input, mode = 'post-tool-use') => execute('runtime/agent-hook.mjs', ['codex', mode], input, f);

test('codex Bash is unknown with empty intent', async t => {
  const f = fixture(t);
  assert.deepEqual(await codexRun(f, codexPayload(f)), { code: 0, stdout: '' });
  const [capture] = captures(f);
  assert.equal(capture.arguments.outcome, 'unknown');
  assert.equal(capture.arguments.origin, 'codex://action');
  const episode = JSON.parse(capture.arguments.content);
  assert.equal(episode.summary, 'cargo test');
  assert.equal(episode.intent, '');
});

test('codex apply_patch yields one call per file', async t => {
  const f = fixture(t);
  const patch = '*** Begin Patch\n*** Add File: a.md\n+content\n*** Update File: src/x.rs\n*** Move to: src/y.rs\n*** End Patch';
  await codexRun(f, codexPayload(f, { tool_name: 'apply_patch', tool_input: { command: patch } }));
  const found = captures(f);
  assert.equal(found.length, 2);
  assert.deepEqual(found.map(c => c.arguments.source), [sourceId('session', 'id#0'), sourceId('session', 'id#1')]);
  const episodes = found.map(c => JSON.parse(c.arguments.content));
  assert.deepEqual(episodes.map(e => e.summary), ['a.md', 'src/y.rs']);
  assert.deepEqual(episodes.map(e => e.seq), [0, 1]);
  const { calls } = await import('../runtime/hook-adapters/codex.mjs');
  assert.deepEqual(calls(codexPayload(f, { tool_name: 'apply_patch', tool_input: { command: patch } })).map(c => c.kind), ['write', 'edit']);
});

test('header-like content lines are ignored', async () => {
  const { patchPaths } = await import('../runtime/hook-adapters/codex.mjs');
  assert.deepEqual(patchPaths('*** Add File: a\n+*** Update File: z'), [{ path: 'a', op: 'add' }]);
});

test('patch without headers records nothing', async t => {
  const f = fixture(t);
  await codexRun(f, codexPayload(f, { tool_name: 'apply_patch', tool_input: { command: '+no headers' } }));
  assert.equal(captures(f).length, 0);
});

test('subagent records nothing', async t => {
  const f = fixture(t);
  await codexRun(f, codexPayload(f, { agent_id: 'a1' }));
  assert.equal(captures(f).length, 0);
});

test('mcp tool records nothing', async t => {
  const f = fixture(t);
  await codexRun(f, codexPayload(f, { tool_name: 'mcp__fs__read' }));
  assert.equal(captures(f).length, 0);
});

test('unknown event records nothing', async t => {
  const f = fixture(t);
  await codexRun(f, codexPayload(f, { hook_event_name: 'Stop' }));
  assert.equal(captures(f).length, 0);
});

test('codex session-start returns additionalContext JSON', async t => {
  const brief = { roots: [{ root: 'Decisions', nodes: [{ title: 'Binding' }] }] };
  const f = fixture(t, { brief });
  const result = await codexRun(f, { ...codexPayload(f), hook_event_name: 'SessionStart', source: 'startup' }, 'session-start');
  assert.equal(result.code, 0);
  assert.deepEqual(JSON.parse(result.stdout), { hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: formatBrief(brief) } });
});

test('more than 8 patch files keeps the first 8 distinct', async () => {
  const { patchPaths } = await import('../runtime/hook-adapters/codex.mjs');
  const patch = ['*** Add File: f0', '*** Update File: f0', ...Array.from({ length: 9 }, (_, i) => `*** Delete File: f${i + 1}`)].join('\n');
  assert.deepEqual(patchPaths(patch), [{ path: 'f0', op: 'add' }, ...Array.from({ length: 7 }, (_, i) => ({ path: `f${i + 1}`, op: 'delete' }))]);
});

test('codex validates identifiers, cwd and session-start event', async () => {
  const adapter = await import('../runtime/hook-adapters/codex.mjs');
  const input = { session_id: 's', tool_use_id: 'id', cwd: '/project', hook_event_name: 'PostToolUse', tool_name: 'Bash', tool_input: { command: 'ls' } };
  for (const invalid of [{ session_id: '' }, { tool_use_id: '' }, { cwd: 'relative' }, { agent_id: null }, { tool_input: {} }]) {
    assert.deepEqual(adapter.calls({ ...input, ...invalid }), []);
  }
  assert.equal(adapter.sessionCwd({ hook_event_name: 'SessionStart', cwd: '/project' }), '/project');
  assert.throws(() => adapter.sessionCwd({ hook_event_name: 'Stop', cwd: '/project' }));
  assert.throws(() => adapter.sessionCwd({ hook_event_name: 'SessionStart', cwd: 'relative' }));
});

test('patch paths deduplicate after moves and preserve the first operation', async () => {
  const { patchPaths } = await import('../runtime/hook-adapters/codex.mjs');
  assert.deepEqual(patchPaths('*** Add File: a\n*** Update File: b\n*** Move to: a\n*** Delete File: c'), [
    { path: 'a', op: 'add' }, { path: 'c', op: 'delete' },
  ]);
});


for (const harness of ['claude-code', 'codex']) {
  const prompt = (f, extra = {}) => ({ hook_event_name: 'UserPromptSubmit', session_id: 'session', cwd: f.project, ...(harness === 'codex' ? { turn_id: 'turn' } : { prompt_id: 'turn' }), prompt: 'hello', ...extra });
  const stop = (f, extra = {}) => ({ hook_event_name: 'Stop', session_id: 'session', cwd: f.project, ...(harness === 'codex' ? { turn_id: 'turn' } : { prompt_id: 'turn' }), last_assistant_message: 'reply', ...extra });
  const run = (f, mode, input) => execute('runtime/agent-hook.mjs', [harness, mode], input, f);
  const stash = f => join(f.runtime, `lambda-memory-${harness === 'codex' ? 'codex' : 'cc'}-prompt-${shortHash('session')}`);
  const enabled = t => fixture(t, { captureConversation: true });

  test(`${harness}: prompt is stashed sanitized with 0600`, async t => {
    const f = enabled(t);
    assert.deepEqual(await run(f, 'user-prompt-submit', prompt(f, { prompt: `token=secretvalue ${f.project}/file ${f.root}/other` })), { code: 0, stdout: '' });
    const saved = readFileSync(stash(f), 'utf8');
    assert.equal(JSON.parse(saved).turnKey, 'turn');
    assert.equal(typeof JSON.parse(saved).prompt, 'string');
    assert.ok(!saved.includes('secretvalue'));
    assert.ok(saved.includes('[REDACTED]'));
    assert.ok(saved.includes('./file'));
    assert.ok(saved.includes('~/other'));
    assert.equal(statSync(stash(f)).mode & 0o777, 0o600);
  });
  test(`${harness}: stop captures exchange and deletes stash`, async t => {
    const f = enabled(t);
    await run(f, 'user-prompt-submit', prompt(f));
    assert.deepEqual(await run(f, 'stop', stop(f, { last_assistant_message: `password=hidden ${f.project}/x` })), { code: 0, stdout: '' });
    assert.equal(existsSync(stash(f)), false);
    const [c] = captures(f);
    assert.equal(c.arguments.kind, 'exchange');
    assert.equal(c.arguments.origin, `${harness}://exchange`);
    assert.equal(c.arguments.outcome, 'unknown');
    assert.equal(c.arguments.source, `${shortHash('session')}/turn-${shortHash('turn')}`);
    assert.deepEqual(JSON.parse(c.arguments.content), { v: 1, prompt: 'hello', reply: 'password=[REDACTED] ./x' });
  });
  test(`${harness}: stop without prompt captures reply only`, async t => {
    const f = enabled(t);
    await run(f, 'stop', stop(f));
    assert.deepEqual(JSON.parse(captures(f)[0].arguments.content), { v: 1, prompt: '', reply: 'reply' });
  });
  test(`${harness}: second prompt replaces stash`, async t => {
    const f = enabled(t);
    await run(f, 'user-prompt-submit', prompt(f, { prompt: 'first' }));
    await run(f, 'user-prompt-submit', prompt(f, { prompt: 'second' }));
    await run(f, 'stop', stop(f));
    assert.equal(JSON.parse(captures(f)[0].arguments.content).prompt, 'second');
  });
  test(`${harness}: subagent, active stop, slash only and other events are skipped`, async t => {
    const f = enabled(t);
    await run(f, 'user-prompt-submit', prompt(f, { agent_id: null }));
    await run(f, 'user-prompt-submit', prompt(f, { prompt: '/help  ' }));
    await run(f, 'user-prompt-submit', prompt(f, { hook_event_name: 'SubagentStop' }));
    assert.equal(existsSync(stash(f)), false);
    await run(f, 'stop', stop(f, { agent_id: 'agent' }));
    await run(f, 'stop', stop(f, { stop_hook_active: true }));
    await run(f, 'stop', stop(f, { hook_event_name: 'SubagentStop' }));
    await run(f, 'stop', stop(f, { last_assistant_message: null }));
    assert.equal(captures(f).length, 0);
  });
  test(`${harness}: capture off when false`, async t => {
    for (const setting of [false, 'true']) {
      const f = fixture(t, { captureConversation: setting });
      await run(f, 'user-prompt-submit', prompt(f));
      await run(f, 'stop', stop(f));
      assert.equal(existsSync(stash(f)), false);
      assert.equal(captures(f).length, 0);
    }
  });
  test(`${harness}: same turn twice has same source and fallback hashes sanitized prompt`, async t => {
    const f = enabled(t);
    for (let i = 0; i < 2; i++) {
      await run(f, 'user-prompt-submit', prompt(f));
      await run(f, 'stop', stop(f));
    }
    assert.equal(captures(f)[0].arguments.source, captures(f)[1].arguments.source);
    if (harness === 'claude-code') {
      await run(f, 'user-prompt-submit', prompt(f, { prompt_id: undefined, prompt: 'token=secret' }));
      await run(f, 'stop', stop(f, { prompt_id: undefined }));
      assert.equal(captures(f)[2].arguments.source, `${shortHash('session')}/turn-${shortHash('token=[REDACTED]')}`);
    }
  });
}

test('captureConversation config defaults to true and rejects non-booleans', t => {
  for (const value of [undefined, false, true, null, 'true', 1]) {
    const f = fixture(t, { captureConversation: value });
    if (value === undefined || typeof value === 'boolean') {
      assert.equal(loadConfig({ LAMBDA_MEMORY_CONFIG: f.config }).captureConversation, value ?? true);
    } else {
      assert.throws(() => loadConfig({ LAMBDA_MEMORY_CONFIG: f.config }), /invalid configuration/);
    }
  }
});


const claudeRun = (f, mode, input, env = {}) => execute('runtime/agent-hook.mjs', ['claude-code', mode], input, f, env);
const briefPayload = { roots: [{ root: 'Decisions', nodes: [{ title: 'Binding' }] }, { root: 'Architecture', nodes: [{ title: 'Hooks' }] }] };
const recallPayload = { hits: [{ node: 'A', path: ['Decisions', 'Binding'], content: 'Use the root' }, { node: 'B', path: ['Architecture'], content: 'Hooks' }] };
const recallInput = (f, extra = {}) => ({ hook_event_name: 'UserPromptSubmit', session_id: 'session', cwd: f.project, prompt: 'Implement the binding for the session root', ...extra });
const recallCalls = f => existsSync(f.log) ? readFileSync(f.log, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse).filter(call => call.name === 'memory_recall') : [];
const recallState = f => loadState(statePath(f.runtime, 'cc', 'session'));

test('session-start injects the brief', async t => {
  const f = fixture(t, { brief: briefPayload });
  const result = await claudeRun(f, 'session-start', { cwd: f.project });
  assert.equal(JSON.parse(result.stdout).hookSpecificOutput.additionalContext, formatBrief(briefPayload));
  const calls = readFileSync(f.log, 'utf8');
});
test('session-start prints nothing for an empty brief', async t => {
  const f = fixture(t, { brief: { roots: [] } });
  assert.deepEqual(await claudeRun(f, 'session-start', { cwd: f.project }), { code: 0, stdout: '' });
});
test('session-start prints nothing when the binary has no memory_brief', async t => {
  const f = fixture(t);
  assert.deepEqual(await claudeRun(f, 'session-start', { cwd: f.project }), { code: 0, stdout: '' });
});
test('recall triggers on the first substantive prompt', async t => {
  const f = fixture(t, { recall: recallPayload });
  const result = await claudeRun(f, 'recall', recallInput(f));
  assert.deepEqual(JSON.parse(result.stdout).hookSpecificOutput, { hookEventName: 'UserPromptSubmit', additionalContext: formatRecall(recallPayload).text });
  assert.deepEqual(recallCalls(f)[0].arguments, { task: recallInput(f).prompt, projects: [`path:${f.project}`], budget_bytes: 6000, exclude: [] });
});
test('recall skips trivial prompts', async t => {
  const f = fixture(t, { recall: recallPayload });
  assert.deepEqual(await claudeRun(f, 'recall', recallInput(f, { prompt: 'ok' })), { code: 0, stdout: '' });
  assert.equal(recallCalls(f).length, 0);
  assert.deepEqual(recallState(f), emptyState());
});
test('recall excludes injected nodes', async t => {
  const f = fixture(t, { recall: recallPayload });
  await claudeRun(f, 'recall', recallInput(f));
  await claudeRun(f, 'recall', recallInput(f, { prompt: 'lembra o que decidimos sobre o binding?' }));
  assert.deepEqual(recallCalls(f)[1].arguments.exclude, ['A', 'B']);
});
test('recall does not repeat without a trigger', async t => {
  const f = fixture(t, { recall: recallPayload });
  await claudeRun(f, 'recall', recallInput(f));
  await claudeRun(f, 'recall', recallInput(f, { prompt: 'Update the binding for the session root' }));
  assert.equal(recallCalls(f).length, 1);
});
test('recall failure continues the turn', async t => {
  const f = fixture(t); writeFileSync(join(f.root, 'bridge.sh'), '#!/bin/sh\nexit 1\n');
  assert.deepEqual(await claudeRun(f, 'recall', recallInput(f)), { code: 0, stdout: '' });
  assert.equal(recallState(f).substantive, 1);
});
test('recall timeout records the attempt', async t => {
  const f = fixture(t); writeFileSync(join(f.root, 'bridge.sh'), '#!/bin/sh\nexec sleep 30\n');
  const started = Date.now();
  assert.deepEqual(await claudeRun(f, 'recall', recallInput(f)), { code: 0, stdout: '' });
  assert.ok(Date.now() - started < 5000);
  assert.equal(recallState(f).substantive, 1);
});
test('recall validates event, session, cwd and prompt', async t => {
  const f = fixture(t, { recall: recallPayload });
  for (const extra of [{ hook_event_name: 'Stop' }, { session_id: '' }, { cwd: 'relative' }, { prompt: null }, { agent_id: null }]) {
    assert.deepEqual(await claudeRun(f, 'recall', recallInput(f, extra)), { code: 0, stdout: '' });
  }
  assert.equal(recallCalls(f).length, 0);
});
test('recall under an ignored root prints nothing', async t => {
  const f = fixture(t, { recall: recallPayload });
  const config = JSON.parse(readFileSync(f.config)); config.ignoreRoots = [f.project]; writeFileSync(f.config, JSON.stringify(config));
  assert.deepEqual(await claudeRun(f, 'recall', recallInput(f)), { code: 0, stdout: '' });
  assert.equal(recallCalls(f).length, 0);
});
test('recall ignores subagents', async t => {
  const f = fixture(t, { recall: recallPayload });
  assert.deepEqual(await claudeRun(f, 'recall', recallInput(f, { agent_id: 'agent' })), { code: 0, stdout: '' });
  assert.equal(recallCalls(f).length, 0);
});
test('pre-compact lists injected nodes and marks compaction', async t => {
  const f = fixture(t, { recall: recallPayload });
  await claudeRun(f, 'recall', recallInput(f));
  const result = await claudeRun(f, 'pre-compact', { hook_event_name: 'PreCompact', session_id: 'session' });
  assert.match(result.stdout, /node IDs: A, B\./);
  assert.equal(recallState(f).compacted, true);
  await claudeRun(f, 'recall', recallInput(f, { prompt: 'Update the binding for the session root' }));
  assert.equal(recallCalls(f).length, 2);
});
test('pre-compact handles missing state and rejects subagents and other events', async t => {
  const f = fixture(t);
  const input = { hook_event_name: 'PreCompact', session_id: 'session' };
  assert.equal((await claudeRun(f, 'pre-compact', input)).stdout, 'Lambda Memory: keep in the summary the decisions recalled from memory in this session and their node IDs.\n');
  for (const extra of [{ agent_id: null }, { hook_event_name: 'Stop' }, { session_id: '' }]) {
    assert.deepEqual(await claudeRun(f, 'pre-compact', { ...input, ...extra }), { code: 0, stdout: '' });
  }
  assert.deepEqual(await codexRun(f, input, 'pre-compact'), { code: 0, stdout: '' });
});
test('codex recall makes no call while prompt injection is unconfirmed', async t => {
  const f = fixture(t, { recall: recallPayload });
  assert.deepEqual(await codexRun(f, recallInput(f), 'recall'), { code: 0, stdout: '' });
  assert.equal(recallCalls(f).length, 0);
});
test('recall sanitizes prompts before persisting and querying', async t => {
  const f = fixture(t, { recall: recallPayload });
  await claudeRun(f, 'recall', recallInput(f, { prompt: `Investigate binding token=secretvalue ${f.project}/file` }));
  assert.ok(!JSON.stringify(recallState(f)).includes('secretvalue'));
  assert.ok(!recallCalls(f)[0].arguments.task.includes('secretvalue'));
  assert.match(recallCalls(f)[0].arguments.task, /\[REDACTED\]/);
});
const subdir = f => { const dir = join(f.project, 'sub', 'dir'); mkdirSync(dir, { recursive: true }); return dir; };

test('claude-code post-tool-use binds to CLAUDE_PROJECT_DIR', async t => {
  const f = fixture(t); const cwd = subdir(f);
  assert.deepEqual(await claudeRun(f, 'post-tool-use', { ...payload(f), cwd }, { CLAUDE_PROJECT_DIR: f.project }), { code: 0, stdout: '' });
  assert.equal(captures(f)[0].arguments.project, `path:${f.project}`);
});
test('claude-code without CLAUDE_PROJECT_DIR binds to the call directory', async t => {
  const f = fixture(t); const cwd = subdir(f);
  await claudeRun(f, 'post-tool-use', { ...payload(f), cwd });
  assert.equal(captures(f)[0].arguments.project, `path:${cwd}`);
});
test('a marker below the session root wins', async t => {
  const f = fixture(t); const cwd = subdir(f);
  writeFileSync(join(f.project, 'sub', 'CLAUDE.md'), 'marker');
  await claudeRun(f, 'post-tool-use', { ...payload(f), cwd }, { CLAUDE_PROJECT_DIR: f.project });
  assert.equal(captures(f)[0].arguments.project, `path:${join(f.project, 'sub')}`);
});
test('ignored cwd captures nothing', async t => {
  const f = fixture(t); const ignored = join(f.root, 'ignored'); const cwd = join(ignored, 'x');
  mkdirSync(cwd, { recursive: true });
  const config = JSON.parse(readFileSync(f.config)); config.ignoreRoots = [ignored]; writeFileSync(f.config, JSON.stringify(config));
  assert.deepEqual(await claudeRun(f, 'post-tool-use', { ...payload(f), cwd }), { code: 0, stdout: '' });
  assert.equal(captures(f).length, 0);
});
test('stop exchange uses the session root project', async t => {
  const f = fixture(t); const cwd = subdir(f); const env = { CLAUDE_PROJECT_DIR: f.project };
  const common = { session_id: 'session', prompt_id: 'p1', cwd };
  await claudeRun(f, 'user-prompt-submit', { ...common, hook_event_name: 'UserPromptSubmit', prompt: 'token=secret' }, env);
  await claudeRun(f, 'stop', { ...common, hook_event_name: 'Stop', last_assistant_message: 'reply' }, env);
  const [capture] = captures(f);
  assert.equal(capture.arguments.project, `path:${f.project}`);
  assert.equal(capture.arguments.source, exchangeSource('session', 'p1', 'token=[REDACTED]'));
});
test('session-start prints the unavailable notice when the bridge fails', async t => {
  const f = fixture(t); writeFileSync(join(f.root, 'bridge.sh'), '#!/bin/sh\nexit 1\n');
  const result = await claudeRun(f, 'session-start', { cwd: f.project });
  assert.equal(result.code, 0);
  assert.deepEqual(JSON.parse(result.stdout).hookSpecificOutput, { hookEventName: 'SessionStart', additionalContext: UNAVAILABLE_CONTEXT });
});
test('session-start prints nothing without configuration', async t => {
  const f = fixture(t);
  assert.deepEqual(await claudeRun(f, 'session-start', { cwd: f.project }, { LAMBDA_MEMORY_CONFIG: join(f.root, 'missing') }), { code: 0, stdout: '' });
});
test('session-start prints nothing under an ignored root', async t => {
  const f = fixture(t); const config = JSON.parse(readFileSync(f.config)); config.ignoreRoots = [f.project]; writeFileSync(f.config, JSON.stringify(config));
  assert.deepEqual(await claudeRun(f, 'session-start', { cwd: f.project }), { code: 0, stdout: '' });
});
// Three git calls of 1.1 s each (under gitAsync's 1.5 s timeout) outlast the 2.7 s work deadline.
test('session-start prints nothing if binding misses the deadline', async t => {
  const f = fixture(t); const bin = join(f.root, 'bin'); mkdirSync(bin);
  writeFileSync(join(bin, 'git'), `#!/bin/sh\nsleep 1.1\nif [ "$3" = rev-parse ]; then printf '%s\\n' '${f.project}'; fi\n`, { mode: 0o755 });
  assert.deepEqual(await claudeRun(f, 'session-start', { cwd: f.project }, { PATH: `${bin}:${process.env.PATH}` }), { code: 0, stdout: '' });
});

test('stop treats an invalid JSON stash as absent', async t => {
  const f = fixture(t); const path = join(f.runtime, `lambda-memory-cc-prompt-${shortHash('session')}`);
  writeFileSync(path, 'old plain text');
  await claudeRun(f, 'stop', { session_id: 'session', cwd: f.project, hook_event_name: 'Stop', prompt_id: 'p1', last_assistant_message: 'reply' });
  assert.deepEqual(JSON.parse(captures(f)[0].arguments.content), { v: 1, prompt: '', reply: 'reply' });
  assert.equal(existsSync(path), false);
});

test('stop pairs a stash when either turn key is null', async t => {
  for (const [promptId, stopId] of [[null, 'p2'], ['p1', null]]) {
    const f = fixture(t); const common = { session_id: 'session', cwd: f.project };
    await claudeRun(f, 'user-prompt-submit', { ...common, hook_event_name: 'UserPromptSubmit', prompt_id: promptId, prompt: 'hello' });
    await claudeRun(f, 'stop', { ...common, hook_event_name: 'Stop', prompt_id: stopId, last_assistant_message: 'reply' });
    assert.equal(JSON.parse(captures(f)[0].arguments.content).prompt, 'hello');
  }
});

test('stop ignores a stash from another turn', async t => {
  const f = fixture(t); const common = { session_id: 'session', cwd: f.project };
  await claudeRun(f, 'user-prompt-submit', { ...common, hook_event_name: 'UserPromptSubmit', prompt_id: 'p1', prompt: 'hello' });
  await claudeRun(f, 'stop', { ...common, hook_event_name: 'Stop', prompt_id: 'p2', last_assistant_message: 'reply' });
  assert.deepEqual(JSON.parse(captures(f)[0].arguments.content), { v: 1, prompt: '', reply: 'reply' });
  assert.equal(existsSync(join(f.runtime, `lambda-memory-cc-prompt-${shortHash('session')}`)), false);
});

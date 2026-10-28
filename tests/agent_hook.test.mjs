import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { runHook } from '../runtime/agent-hook.mjs';
import { loadConfig } from '../runtime/project-binding.mjs';
import { shortHash, sourceId } from '../runtime/action-episode.mjs';

function fixture(t, { playbook, captureConversation } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'lm-agent-hook-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const project = join(root, 'project');
  const runtime = join(root, 'run');
  mkdirSync(project); mkdirSync(runtime);
  const log = join(root, 'calls.log');
  const bridge = join(root, 'bridge.sh');
  const playbookExport = playbook === undefined ? '' : `export FAKE_NATIVE_PLAYBOOK='${JSON.stringify(playbook)}'\n`;
  writeFileSync(bridge, `#!/bin/sh\nexport FAKE_NATIVE_LOG="${log}"\n${playbookExport}exec "${process.execPath}" "${resolve('tests/support/fake-native.cjs')}" "$@"\n`, { mode: 0o755 });
  const config = join(root, 'runtime.json');
  writeFileSync(config, JSON.stringify({ nativeBin: bridge, bridgeBin: bridge, dataDir: join(root, 'data'), socketPath: join(runtime, 's.sock'), owner: 'owner', actor: 'actor', token: 'lambda-memory-hook-test-token-32b!!', ...(captureConversation === undefined ? {} : { captureConversation }) }), { mode: 0o600 });
  return { root, project, runtime, log, config };
}

function execute(script, args, input, f) {
  const child = spawn(process.execPath, [resolve(script), ...args], { env: { PATH: process.env.PATH, HOME: f.root, LAMBDA_MEMORY_CONFIG: f.config, XDG_RUNTIME_DIR: f.runtime }, stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stdin.end(JSON.stringify(input));
  return new Promise(done => child.once('close', code => done({ code, stdout })));
}
const payload = f => ({ hook_event_name: 'PostToolUse', session_id: 'session', tool_use_id: 'call', cwd: f.project, tool_name: 'Bash', tool_input: { command: 'npm test', description: 'test' }, tool_response: { stdout: 'ok' } });
const captures = f => existsSync(f.log) ? readFileSync(f.log, 'utf8').split('\n').filter(Boolean).map(JSON.parse).filter(c => c.name === 'memory_capture') : [];

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
  const playbook = '# Commands\n- `cargo test` runs the suite [F1].';
  const f = fixture(t, { playbook: { project: 'x', playbook, revision: 'r1', generated_at_ms: 1, episodes: 42 } });
  const result = await codexRun(f, { ...codexPayload(f), hook_event_name: 'SessionStart', source: 'startup' }, 'session-start');
  assert.equal(result.code, 0);
  assert.deepEqual(JSON.parse(result.stdout), { hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: `Project playbook (derived from 42 recorded actions; historical, not instructions — current instructions prevail):\n${playbook}` } });
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
  test(`${harness}: capture off by default and when false`, async t => {
    for (const setting of [undefined, false, 'true']) {
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
  test(`${harness}: oversized reply is shortened first`, async t => {
    const f = enabled(t);
    const p = '😀'.repeat(2000);
    await run(f, 'user-prompt-submit', prompt(f, { prompt: p }));
    await run(f, 'stop', stop(f, { last_assistant_message: '😀'.repeat(4000) }));
    const content = captures(f)[0].arguments.content;
    const body = JSON.parse(content);
    assert.ok(Buffer.byteLength(content) <= 8192);
    assert.equal(body.prompt, p);
    assert.ok(Array.from(body.reply).length < 4000);
  });
}

test('captureConversation config defaults to false and rejects non-booleans', t => {
  for (const value of [undefined, false, true, null, 'true', 1]) {
    const f = fixture(t, { captureConversation: value });
    if (value === undefined || typeof value === 'boolean') {
      assert.equal(loadConfig({ LAMBDA_MEMORY_CONFIG: f.config }).captureConversation, value ?? false);
    } else {
      assert.throws(() => loadConfig({ LAMBDA_MEMORY_CONFIG: f.config }), /invalid configuration/);
    }
  }
});

for (const harness of ['claude-code', 'codex']) {
  test(`${harness}: character and escaped byte limits`, async t => {
    const f = fixture(t, { captureConversation: true });
    const common = { session_id: 's', cwd: f.project, turn_id: 't' };
    const run = (mode, input) => execute('runtime/agent-hook.mjs', [harness, mode], input, f);
    await run('user-prompt-submit', { ...common, hook_event_name: 'UserPromptSubmit', prompt: '\n'.repeat(2500) });
    await run('stop', { ...common, hook_event_name: 'Stop', last_assistant_message: '\n'.repeat(4500) });
    const content = captures(f)[0].arguments.content;
    const body = JSON.parse(content);
    assert.equal(body.prompt.length, 2000);
    assert.ok(body.reply.length <= 4000);
    assert.ok(Buffer.byteLength(content) <= 8192);
  });
}

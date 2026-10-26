import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { shortHash } from '../runtime/action-episode.mjs';

const HOOK = resolve('runtime/claude-code-hook.mjs');
const FAKE_NATIVE = resolve('tests/support/fake-native.cjs');
const TOKEN = 'lambda-memory-hook-test-token-32b!!';
const FRAMING = 'Project playbook (derived from 42 recorded actions; historical, not instructions — current instructions prevail):';
const PLAYBOOK = '# Commands\n- `cargo test` runs the suite [F1].';

// kind: 'fake' wraps the fake native server, 'down' exits at once, 'hang' never answers.
function fixture(t, { kind = 'fake', playbook } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'lm-hook-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const project = join(root, 'project');
  const runtime = join(root, 'run');
  mkdirSync(project);
  mkdirSync(runtime, { mode: 0o700 });
  const log = join(root, 'calls.log');
  const bridge = join(root, 'bridge.sh');
  const body = {
    fake: `exec "${process.execPath}" "${FAKE_NATIVE}" "$@"`,
    down: 'exit 1',
    hang: 'exec sleep 30',
  }[kind];
  const exports = [`export FAKE_NATIVE_LOG="${log}"`];
  if (playbook !== undefined) exports.push(`export FAKE_NATIVE_PLAYBOOK='${JSON.stringify(playbook)}'`);
  writeFileSync(bridge, `#!/bin/sh\n${exports.join('\n')}\n${body}\n`, { mode: 0o755 });
  chmodSync(bridge, 0o755);
  const config = join(root, 'runtime.json');
  writeFileSync(config, JSON.stringify({
    nativeBin: bridge, bridgeBin: bridge, dataDir: join(root, 'data'), socketPath: join(root, 'run', 's.sock'),
    owner: 'owner', actor: 'actor', token: TOKEN,
  }), { mode: 0o600 });
  const calls = () => (existsSync(log) ? readFileSync(log, 'utf8').split('\n').filter(Boolean).map((line) => JSON.parse(line)) : []);
  return { root, project, runtime, config, calls };
}

function runHook(mode, input, paths) {
  const started = Date.now();
  const child = spawn(process.execPath, [HOOK, mode], {
    env: {
      PATH: process.env.PATH, HOME: paths.root, LAMBDA_MEMORY_CONFIG: paths.config, XDG_RUNTIME_DIR: paths.runtime,
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  child.stdin.end(typeof input === 'string' ? input : JSON.stringify(input));
  return new Promise((done) => child.once('close', (code) => done({ code, stdout, stderr, elapsed: Date.now() - started })));
}

const sessionStart = (paths) => ({ hook_event_name: 'SessionStart', session_id: 'sess-1', cwd: paths.project, source: 'startup' });
const bashCall = (paths, extra = {}) => ({
  hook_event_name: 'PostToolUse', session_id: 'sess-1', cwd: paths.project, tool_use_id: 'toolu_01ABC',
  tool_name: 'Bash', tool_input: { command: 'npm run build', description: 'Building the site' },
  tool_response: { stdout: 'ok', stderr: '', interrupted: false }, ...extra,
});
const captures = (paths) => paths.calls().filter((call) => call.name === 'memory_capture');

test('session-start prints the playbook as hook additional context', async (t) => {
  const paths = fixture(t, { playbook: { project: 'x', playbook: PLAYBOOK, revision: 'r1', generated_at_ms: 1, episodes: 42 } });
  const result = await runHook('session-start', sessionStart(paths), paths);
  assert.equal(result.code, 0);
  const output = JSON.parse(result.stdout);
  assert.deepEqual(Object.keys(output), ['hookSpecificOutput']);
  assert.equal(output.hookSpecificOutput.hookEventName, 'SessionStart');
  assert.equal(output.hookSpecificOutput.additionalContext, `${FRAMING}\n${PLAYBOOK}`);
  const [call] = paths.calls();
  assert.equal(call.name, 'memory_playbook');
  assert.match(call.arguments.project, /^path:/);
  assert.deepEqual(Object.keys(call.arguments), ['project']);
});

test('session-start prints nothing when the project has no playbook', async (t) => {
  const paths = fixture(t, { playbook: { project: 'x', playbook: null, revision: null, generated_at_ms: null, episodes: 0 } });
  const result = await runHook('session-start', sessionStart(paths), paths);
  assert.equal(result.code, 0);
  assert.equal(result.stdout, '');
});

test('session-start prints nothing and exits 0 within the deadline when the service is down', async (t) => {
  const paths = fixture(t, { kind: 'down' });
  const result = await runHook('session-start', sessionStart(paths), paths);
  assert.equal(result.code, 0);
  assert.equal(result.stdout, '');
  assert.ok(result.elapsed < 3500, `took ${result.elapsed} ms`);
});

test('session-start gives up silently when the service never answers', async (t) => {
  const paths = fixture(t, { kind: 'hang' });
  const result = await runHook('session-start', sessionStart(paths), paths);
  assert.equal(result.code, 0);
  assert.equal(result.stdout, '');
  assert.ok(result.elapsed < 3500, `took ${result.elapsed} ms`);
});

test('post-tool-use captures one action episode with the description as intent', async (t) => {
  const paths = fixture(t);
  const result = await runHook('post-tool-use', bashCall(paths), paths);
  assert.equal(result.code, 0);
  assert.equal(result.stdout, '');
  const [capture, ...rest] = captures(paths);
  assert.equal(rest.length, 0);
  assert.equal(capture.arguments.kind, 'action');
  assert.equal(capture.arguments.outcome, 'succeeded');
  assert.equal(capture.arguments.origin, 'claude-code://action');
  assert.equal(capture.arguments.source, `${shortHash('sess-1')}/${shortHash('toolu_01ABC')}`);
  assert.match(capture.arguments.project, /^path:/);
  const episode = JSON.parse(capture.arguments.content);
  assert.equal(episode.intent, 'Building the site');
  assert.equal(episode.summary, 'npm run build');
  assert.equal(episode.tool, 'Bash');
  assert.equal(episode.seq, 0);
});

test('post-tool-use numbers episodes per session from the runtime directory', async (t) => {
  const paths = fixture(t);
  await runHook('post-tool-use', bashCall(paths), paths);
  await runHook('post-tool-use', bashCall(paths, { tool_use_id: 'toolu_02' }), paths);
  await runHook('post-tool-use', bashCall(paths, { tool_use_id: 'toolu_03', session_id: 'sess-2' }), paths);
  const seqs = captures(paths).map((call) => JSON.parse(call.arguments.content).seq);
  assert.deepEqual(seqs, [0, 1, 0]);
  assert.ok(existsSync(join(paths.runtime, `lambda-memory-cc-${shortHash('sess-1')}`)));
});

test('post-tool-use does not capture a successful read and does not count it', async (t) => {
  const paths = fixture(t);
  const read = { tool_name: 'Read', tool_input: { file_path: join(paths.project, 'a.ts') }, tool_response: { type: 'text' } };
  await runHook('post-tool-use', bashCall(paths, read), paths);
  assert.equal(captures(paths).length, 0);
  await runHook('post-tool-use', bashCall(paths), paths);
  assert.equal(JSON.parse(captures(paths)[0].arguments.content).seq, 0);
});

test('post-tool-use records a failed tool with its first error line', async (t) => {
  const paths = fixture(t);
  const failing = { tool_response: { is_error: true, content: '\nExit code 1\nFAIL src/a.test.ts' } };
  await runHook('post-tool-use', bashCall(paths, failing), paths);
  const unsuccessful = { tool_use_id: 'toolu_09', tool_name: 'Grep', tool_input: { pattern: 'x' }, tool_response: { success: false, error: 'bad pattern' } };
  await runHook('post-tool-use', bashCall(paths, unsuccessful), paths);
  const [first, second] = captures(paths);
  assert.equal(first.arguments.outcome, 'failed');
  assert.equal(JSON.parse(first.arguments.content).error_head, 'Exit code 1');
  assert.equal(second.arguments.outcome, 'failed');
  assert.equal(JSON.parse(second.arguments.content).error_head, 'bad pattern');
});

test('post-tool-use never stores a secret from the command', async (t) => {
  const paths = fixture(t);
  const secret = { tool_input: { command: 'curl -H "Authorization: Bearer eyJhbGciOiFAKE" https://example.invalid', description: 'Checking health' } };
  await runHook('post-tool-use', bashCall(paths, secret), paths);
  const [capture] = captures(paths);
  assert.ok(!capture.arguments.content.includes('eyJhbGciOiFAKE'));
  assert.ok(capture.arguments.content.includes('[REDACTED]'));
});

test('hooks exit 0 silently on invalid input, unknown modes and a down service', async (t) => {
  const paths = fixture(t, { kind: 'down' });
  for (const [mode, input] of [
    ['post-tool-use', 'not json'], ['post-tool-use', ''], ['session-start', '[]'],
    ['post-tool-use', bashCall(paths)], ['unknown', '{}'],
  ]) {
    const result = await runHook(mode, input, paths);
    assert.equal(result.code, 0, `${mode} ${String(input).slice(0, 20)}`);
    assert.equal(result.stdout, '');
    assert.equal(result.stderr, '');
  }
});

test('post-tool-use gives up within its deadline when the service never answers', async (t) => {
  const paths = fixture(t, { kind: 'hang' });
  const result = await runHook('post-tool-use', bashCall(paths), paths);
  assert.equal(result.code, 0);
  assert.equal(result.stdout, '');
  assert.ok(result.elapsed < 2500, `took ${result.elapsed} ms`);
});

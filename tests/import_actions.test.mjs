import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { buildEpisode, sourceId } from '../runtime/action-episode.mjs';
import { describeOmp } from '../runtime/hook-adapters/tool-kinds.mjs';
import { readClaudeCalls, readOmpCalls, transcriptDir } from '../runtime/transcripts.mjs';

const IMPORTER = resolve('runtime/import-actions.mjs');
const FAKE_NATIVE = resolve('tests/support/fake-native.cjs');
const FIXTURES = resolve('tests/fixtures/transcripts');
const OMP_FIXTURE = join(FIXTURES, 'omp-session.jsonl');
const CLAUDE_FIXTURE = join(FIXTURES, 'claude-session.jsonl');
const OMP_SESSION = '11111111-2222-4333-8444-555555555555';
const CLAUDE_SESSION = 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee';
const TOKEN = 'lambda-memory-import-test-token-32b!';

async function collect(iterable) {
  const items = [];
  for await (const item of iterable) items.push(item);
  return items;
}

// A fake home holding one transcript per agent for a real (temporary) project directory.
function fixture(t, { reject } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'lm-import-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const home = join(root, 'home');
  const project = join(root, 'project');
  mkdirSync(project, { recursive: true });
  mkdirSync(home);
  for (const [agent, file] of [['omp', OMP_FIXTURE], ['claude-code', CLAUDE_FIXTURE]]) {
    const dir = transcriptDir(agent, project, home);
    mkdirSync(dir, { recursive: true });
    copyFileSync(file, join(dir, 'session.jsonl'));
  }
  const store = join(root, 'store.txt');
  const log = join(root, 'calls.log');
  const bridge = join(root, 'bridge.sh');
  const exports = [`export FAKE_NATIVE_STORE="${store}"`, `export FAKE_NATIVE_LOG="${log}"`];
  if (reject) exports.push(`export FAKE_NATIVE_REJECT='${reject}'`);
  writeFileSync(bridge, `#!/bin/sh\n${exports.join('\n')}\nexec "${process.execPath}" "${FAKE_NATIVE}" "$@"\n`, { mode: 0o755 });
  chmodSync(bridge, 0o755);
  const config = join(root, 'runtime.json');
  writeFileSync(config, JSON.stringify({
    nativeBin: bridge, bridgeBin: bridge, dataDir: join(root, 'data'), socketPath: join(root, 'run', 's.sock'),
    owner: 'owner', actor: 'actor', token: TOKEN,
  }), { mode: 0o600 });
  const calls = () => (existsSync(log) ? readFileSync(log, 'utf8').split('\n').filter(Boolean).map((line) => JSON.parse(line)) : []);
  return { root, home, project, config, calls };
}

function run(paths, args) {
  const child = spawn(process.execPath, [IMPORTER, ...args], {
    env: { PATH: process.env.PATH, HOME: paths.home, LAMBDA_MEMORY_CONFIG: paths.config },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  return new Promise((done) => child.once('close', (code) => done({ code, stdout, stderr })));
}

test('transcriptDir encodes the project directory for each agent', () => {
  const home = homedir();
  assert.equal(transcriptDir('omp', `${home}/Projects/My App`), join(home, '.omp/agent/sessions', '-Projects-My App'));
  assert.equal(transcriptDir('omp', '/h/Projects/p', '/h'), '/h/.omp/agent/sessions/-Projects-p');
  assert.equal(transcriptDir('omp', '/h', '/h'), '/h/.omp/agent/sessions/-');
  assert.equal(transcriptDir('claude-code', '/home/u/Projects/my.app', '/h'), '/h/.claude/projects/-home-u-Projects-my-app');
  assert.throws(() => transcriptDir('vim', '/x'), /agent/);
});

test('readOmpCalls pairs calls with results by ID and counts what it skips', async () => {
  const stats = {};
  const calls = await collect(readOmpCalls(OMP_FIXTURE, stats));
  assert.deepEqual(calls.map((call) => call.tool), ['bash', 'read', 'edit', 'bash', 'bash']);
  assert.deepEqual(calls.map((call) => call.seq), [0, 1, 2, 3, 4]);
  assert.deepEqual(stats, { malformed: 1, unpaired: 1 });
  const [first, , edit, failed] = calls;
  assert.equal(first.sessionId, OMP_SESSION);
  assert.equal(first.callId, 'call_AAAA1111|fc_BBBB2222');
  assert.equal(first.input.i, 'Running tests');
  assert.equal(first.isError, false);
  assert.equal(first.errorText, '');
  assert.equal(first.timestampMs, Date.parse('2026-10-01T10:00:05.000Z'));
  assert.equal(edit.input.path, '/home/u/Projects/p/src/a.rs');
  assert.equal(failed.isError, true);
  assert.match(failed.errorText, /unresolved import/);
});

test('readClaudeCalls pairs tool_use with tool_result and reads both content shapes', async () => {
  const stats = {};
  const calls = await collect(readClaudeCalls(CLAUDE_FIXTURE, stats));
  assert.deepEqual(calls.map((call) => [call.tool, call.callId]), [
    ['Bash', 'toolu_01AAAA'], ['Read', 'toolu_01CCCC'], ['Edit', 'toolu_01BBBB'], ['Bash', 'toolu_01DDDD'],
  ]);
  assert.deepEqual(stats, { malformed: 1, unpaired: 1 });
  assert.ok(calls.every((call) => call.sessionId === CLAUDE_SESSION));
  assert.equal(calls[0].input.description, 'Building the site');
  assert.equal(calls[3].isError, true);
  assert.match(calls[3].errorText, /Exit code 1/);
  assert.equal(calls[0].timestampMs, Date.parse('2026-10-02T09:00:04.000Z'));
});

test('dry run counts calls, episodes and skipped without touching the service', async (t) => {
  const paths = fixture(t);
  const result = await run(paths, ['--agent', 'omp', '--cwd', paths.project]);
  assert.equal(result.code, 0, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.deepEqual(Object.keys(report), ['calls', 'episodes', 'skipped', 'samples']);
  assert.equal(report.calls, 6);
  assert.equal(report.episodes, 4);
  assert.deepEqual(report.skipped, { unpaired_call: 1, no_episode: 1, malformed_line: 1 });
  assert.equal(report.samples.length, 4);
  assert.equal(paths.calls().length, 0);
  const text = JSON.stringify(report);
  assert.ok(!text.includes('eyJhbGciOiFAKE'));
  assert.ok(text.includes('[REDACTED]'));
  const build = report.samples.find((sample) => JSON.parse(sample.content).summary === 'cargo test --features memory-runtime,router-http');
  assert.equal(build.origin, 'import://omp');
  assert.equal(build.kind, 'action');
  assert.equal(build.recorded_at_ms, Date.parse('2026-10-01T10:00:05.000Z'));
  assert.equal(JSON.parse(build.content).intent, 'Running tests');
});

test('dry run limits samples to 10 and honours --since', async (t) => {
  const paths = fixture(t);
  const claude = await run(paths, ['--agent', 'claude-code', '--cwd', paths.project]);
  const report = JSON.parse(claude.stdout);
  assert.equal(report.calls, 5);
  assert.equal(report.episodes, 3);
  assert.deepEqual(report.skipped, { unpaired_call: 1, no_episode: 1, malformed_line: 1 });
  assert.equal(report.samples.every((sample) => sample.origin === 'import://claude-code'), true);
  const since = await run(paths, ['--agent', 'omp', '--cwd', paths.project, '--since', '2026-10-01T10:00:10Z']);
  const later = JSON.parse(since.stdout);
  assert.equal(later.episodes, 2); // the failed build (10:00:12) and the curl (10:00:14)
  assert.equal(later.skipped.before_since, 3);
});

test('an imported episode has the same source as a live one for the same call', async (t) => {
  const paths = fixture(t);
  const report = JSON.parse((await run(paths, ['--agent', 'omp', '--cwd', paths.project])).stdout);
  const sample = report.samples.find((item) => JSON.parse(item.content).summary === 'cargo test --features memory-runtime,router-http');
  const live = buildEpisode({
    tool: 'bash', ...describeOmp('bash', { command: 'cargo test --features memory-runtime,router-http\nmore', i: 'Running tests' }),
    outcome: 'succeeded', errorText: '', sessionId: OMP_SESSION, callId: 'call_AAAA1111|fc_BBBB2222', seq: 0, home: homedir(), root: paths.project,
  });
  assert.equal(sample.source, live.source);
  assert.equal(sample.source, sourceId(OMP_SESSION, 'call_AAAA1111|fc_BBBB2222'));
  assert.match(sample.source, /^[0-9a-f]{16}\/[0-9a-f]{16}$/);
  assert.equal(sample.content, live.content);
});

test('--apply twice saves once and then reports already saved', async (t) => {
  const paths = fixture(t);
  const args = ['--agent', 'omp', '--cwd', paths.project, '--apply'];
  const first = await run(paths, args);
  assert.equal(first.code, 0, first.stderr);
  assert.deepEqual(JSON.parse(first.stdout), { saved: 4, already_saved: 0, rejected: 0 });
  const second = await run(paths, args);
  assert.equal(second.code, 0, second.stderr);
  assert.deepEqual(JSON.parse(second.stdout), { saved: 0, already_saved: 4, rejected: 0 });
  const captures = paths.calls().filter((call) => call.name === 'memory_capture');
  assert.equal(captures.length, 8);
  const [capture] = captures;
  assert.deepEqual(Object.keys(capture.arguments).sort(), ['content', 'kind', 'origin', 'outcome', 'project', 'recorded_at_ms', 'source']);
  assert.equal(capture.arguments.kind, 'action');
  assert.equal(capture.arguments.origin, 'import://omp');
  assert.match(capture.arguments.project, /^path:/);
  assert.deepEqual(captures.slice(0, 4).map((call) => call.arguments.outcome), ['succeeded', 'succeeded', 'failed', 'succeeded']);
  assert.deepEqual(captures.slice(0, 4).map((call) => JSON.parse(call.arguments.content).seq), [0, 1, 2, 3]);
});

test('--apply sends the transcript timestamp as recorded_at_ms', async (t) => {
  const paths = fixture(t);
  const dry = await run(paths, ['--agent', 'omp', '--cwd', paths.project]);
  assert.equal(dry.code, 0, dry.stderr);
  const samples = JSON.parse(dry.stdout).samples;
  assert.equal(samples.length, 4);
  const applied = await run(paths, ['--agent', 'omp', '--cwd', paths.project, '--apply']);
  assert.equal(applied.code, 0, applied.stderr);
  const captures = paths.calls().filter((call) => call.name === 'memory_capture');
  assert.equal(captures.length, 4);
  assert.equal(captures[0].arguments.recorded_at_ms, Date.parse('2026-10-01T10:00:05.000Z'));
  assert.deepEqual(captures.map((call) => call.arguments.recorded_at_ms), samples.map((sample) => sample.recorded_at_ms));
  assert.deepEqual(captures.map((call) => call.arguments.source), samples.map((sample) => sample.source));
  assert.ok(captures.every((call) => Number.isSafeInteger(call.arguments.recorded_at_ms)));
});

test('--apply imports Claude Code history and counts rejected episodes', async (t) => {
  const paths = fixture(t, { reject: 'npm test' });
  const result = await run(paths, ['--agent', 'claude-code', '--cwd', paths.project, '--apply']);
  assert.equal(result.code, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), { saved: 2, already_saved: 0, rejected: 1 });
  const origins = new Set(paths.calls().filter((call) => call.name === 'memory_capture').map((call) => call.arguments.origin));
  assert.deepEqual([...origins], ['import://claude-code']);
});

test('bad arguments fail with a usage error and write nothing to stdout', async (t) => {
  const paths = fixture(t);
  for (const args of [[], ['--agent', 'vim', '--cwd', paths.project], ['--agent', 'omp'], ['--agent', 'omp', '--cwd', paths.project, '--since', 'yesterday'],
    ['--agent', 'omp', '--cwd', paths.project, '--bogus']]) {
    const result = await run(paths, args);
    assert.equal(result.code, 2, args.join(' '));
    assert.equal(result.stdout, '');
    assert.match(result.stderr, /usage|invalid/i);
  }
});

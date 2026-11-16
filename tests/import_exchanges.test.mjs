import assert from 'node:assert/strict';
import { test } from 'node:test';
import { appendFileSync, existsSync, readFileSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { readClaudeTurns, readOmpTurns, transcriptDir } from '../runtime/transcripts.mjs';
import { exchangeSource } from '../runtime/exchange.mjs';
import { shortHash } from '../runtime/action-episode.mjs';

async function collect(generator) { const items = []; for await (const item of generator) items.push(item); return items; }
function transcript(t, records) {
  const dir = mkdtempSync(join(tmpdir(), 'lm-turns-')); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const file = join(dir, 'session.jsonl'); writeFileSync(file, records.map(x => JSON.stringify(x)).join('\n')); return file;
}
const timestamp = new Date(Date.now() - 10000).toISOString();
const user = (text, id = 'p', extra = {}) => ({ type: 'user', promptId: id, sessionId: 's', cwd: '/project', timestamp, message: { role: 'user', content: text }, ...extra });
const assistant = text => ({ type: 'assistant', timestamp, message: { content: [{ type: 'text', text }] } });

test('claude turns pair prompts with the last assistant reply', async t => {
  const file = transcript(t, [user('hello'), assistant('first'), user([{ type: 'tool_result', content: 'x' }]), assistant('last'), user('<task-notification>x'), assistant('wrong'), user('meta', 'm', { isMeta: true }), user('side', 's', { isSidechain: true }), user('compact', 'c', { isCompactSummary: true }), user('/foo', 'x')]);
  const turns = await collect(readClaudeTurns(file, {})); assert.equal(turns.length, 1); assert.equal(turns[0].reply, 'last');
});

test('omp turns use the user entry id', async t => {
  const file = transcript(t, [{ type: 'session', id: 'session', cwd: '/project', timestamp }, { type: 'message', id: 'u1', timestamp, message: { role: 'user', content: [{ type: 'text', text: 'hello' }] } }, { type: 'message', timestamp, message: { role: 'assistant', content: [{ type: 'thinking', text: 'hidden' }, { type: 'toolCall', text: 'tool' }, { type: 'text', text: 'reply' }] } }]);
  const turns = await collect(readOmpTurns(file, {})); assert.equal(turns[0].turnKey, 'omp-entry-u1'); assert.equal(turns[0].reply, 'reply');
  const stats = {}; assert.deepEqual(await collect(readOmpTurns(transcript(t, [{ type: 'session', timestamp, parentSession: 'parent' }]), stats)), []); assert.equal(stats.subagent_session, 1);
});

test('slash commands with arguments are imported', async t => {
  const turns = await collect(readClaudeTurns(transcript(t, [user('<command-name>/implement</command-name><command-args>PBI-001</command-args>'), user('<command-name>/implement</command-name><command-args></command-args>', '2')]), {}));
  assert.deepEqual(turns.map(x => x.prompt), ['/implement PBI-001']);
});

test('an omp file whose first line is a title record', async t => {
  const file = transcript(t, [{ type: 'title', timestamp }, { type: 'session', id: 's', cwd: '/project', timestamp }, { type: 'message', id: 'u', timestamp, message: { role: 'user', content: [{ type: 'text', text: 'hi' }] } }]);
  assert.equal((await collect(readOmpTurns(file, {})))[0].sessionId, 's');
});

const IMPORTER = resolve('runtime/import-exchanges.mjs');
const NATIVE = resolve('tests/support/fake-native.cjs');
function fixture(t, agent = 'claude-code') {
  const root = mkdtempSync(join(tmpdir(), 'lm-import-ex-')); t.after(() => rmSync(root, { recursive: true, force: true }));
  const home = join(root, 'home'), project = join(root, 'project'); mkdirSync(home); mkdirSync(project);
  const dir = transcriptDir(agent, project, home); mkdirSync(dir, { recursive: true });
  const bridge = join(root, 'bridge.sh'), store = join(root, 'store'), log = join(root, 'log'), config = join(root, 'runtime.json');
  const updateBridge = (extra = {}) => {
    const vars = { FAKE_NATIVE_STORE: store, FAKE_NATIVE_LOG: log, ...extra };
    writeFileSync(bridge, `#!/bin/sh\n${Object.entries(vars).map(([k,v]) => `export ${k}='${v}'`).join('\n')}\nexec '${process.execPath}' '${NATIVE}' "$@"\n`, { mode: 0o755 });
  };
  updateBridge();
  writeFileSync(config, JSON.stringify({ nativeBin: bridge, bridgeBin: bridge, dataDir: join(root, 'data'), socketPath: join(root, 'sock'), owner: 'owner', actor: 'actor', token: 'lambda-memory-import-test-token-32b!', ignoreRoots: [] }), { mode: 0o600 });
  const file = (name, records) => { const path = join(dir, name + '.jsonl'); writeFileSync(path, records.map(x => JSON.stringify(x)).join('\n') + '\n'); return path; };
  const calls = () => existsSync(log) ? readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
  return { root, home, project, agent, config, file, calls, updateBridge };
}
function run(f, extra = [], input) {
  const child = spawn(process.execPath, [IMPORTER, '--agent', f.agent, '--cwd', f.project, ...extra], { env: { PATH: process.env.PATH, HOME: f.home, LAMBDA_MEMORY_CONFIG: f.config }, stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '', stderr = ''; child.stdout.on('data', x => stdout += x); child.stderr.on('data', x => stderr += x); child.stdin.end(input === undefined ? '' : JSON.stringify(input));
  return new Promise(done => child.on('close', code => done({ code, stdout, stderr, json: () => JSON.parse(stdout) })));
}
function prompt(f, text, id = 'p', extra = {}) { return user(text, id, { cwd: f.project, ...extra }); }
async function dry(f, args = []) { const r = await run(f, args); assert.equal(r.code, 0, r.stderr); return r.json(); }
async function apply(f, plan, args = []) { return run(f, [...args, '--apply'], { ...plan, accept_charges: true }); }

test('a session with a secret-like turn is excluded and listed', async t => {
  const f = fixture(t), key = 'glsa_' + 'A'.repeat(32);
  f.file('a', [prompt(f, `first ${f.project}`), assistant('reply'), prompt(f, key, 'p2')]);
  const plan = await dry(f); assert.equal(plan.turns, 0); assert.equal(plan.excluded_flagged.length, 1);
  assert.equal(plan.excluded_flagged[0].first_at, timestamp); assert.equal(plan.excluded_flagged[0].first_prompt, 'first .'); assert.equal(plan.excluded_flagged[0].session, shortHash('s'));
  const included = await dry(f, ['--include', shortHash('s')]); assert.equal(included.turns, 2); assert.deepEqual(included.included_flagged, [shortHash('s')]); assert.ok(!JSON.stringify(included).includes(key));
  assert.equal((await run(f, ['--include', 'bad'])).code, 2);
});

test('turns appended after the dry run do not change the plan', async t => {
  const f = fixture(t); const file = f.file('a', [prompt(f, 'first')]); const plan = await dry(f);
  appendFileSync(file, JSON.stringify(prompt(f, 'later', 'later', { timestamp: new Date(plan.cutoff_ms + 1000).toISOString() })) + '\n');
  const r = await apply(f, plan); assert.equal(r.code, 0, r.stderr); assert.equal(r.json().saved, 1); assert.equal(f.calls().length, 1);
});

test('a turn captured live counts as already_live', async t => {
  const f = fixture(t); f.file('a', [prompt(f, 'hello')]); const plan = await dry(f); f.updateBridge({ FAKE_NATIVE_CONFLICT: plan.samples[0].source });
  const r = await apply(f, plan); assert.equal(r.code, 0, r.stderr); assert.equal(r.json().already_live, 1); assert.equal(r.json().outcome, 'completed');
});

test('the same promptId in two files yields one source', async t => {
  const f = fixture(t); f.file('a', [prompt(f, 'late', 'same', { sessionId: 'late', timestamp: new Date(Date.now() - 1000).toISOString() })]);
  f.file('b', [prompt(f, 'early', 'same', { sessionId: 'early' })]); const plan = await dry(f);
  assert.equal(plan.turns, 1); assert.equal(plan.samples[0].source, exchangeSource('early', 'same', 'early'));
});

test('old and other-project turns are skipped and counted', async t => {
  const f = fixture(t), child = join(f.project, 'child'); mkdirSync(child); assert.equal(spawnSync('git', ['init', '-q', child]).status, 0);
  f.file('a', [prompt(f, 'old', 'old', { timestamp: new Date(Date.now() - 400 * 86400000).toISOString() }), prompt(f, 'other', 'other', { cwd: child }), prompt(f, 'kept', 'kept')]);
  const plan = await dry(f); assert.equal(plan.turns, 1); assert.equal(plan.skipped.too_old, 1); assert.equal(plan.skipped.other_project, 1);
});

test('apply requires the exact plan with accept_charges true', async t => {
  const f = fixture(t); f.file('a', [prompt(f, 'hello')]); const plan = await dry(f);
  for (const invalid of [{ ...plan, turns: 3, accept_charges: true }, plan]) {
    const r = await run(f, ['--apply'], invalid); assert.equal(r.code, 2); assert.match(r.stderr, /exact import plan consent required/); assert.deepEqual(f.calls(), []);
  }
});

test('apply captures with import origin and transcript time', async t => {
  for (const agent of ['claude-code', 'omp']) {
    const f = fixture(t, agent);
    if (agent === 'claude-code') f.file('a', [prompt(f, 'hello')]);
    else f.file('a', [{ type: 'session', id: 's', cwd: f.project, timestamp }, { type: 'message', id: 'p', timestamp, message: { role: 'user', content: [{ type: 'text', text: 'hello' }] } }]);
    const plan = await dry(f), result = await apply(f, plan); assert.equal(result.code, 0, result.stderr);
    const capture = f.calls()[0].arguments; assert.equal(capture.origin, `import://${agent}`); assert.equal(capture.kind, 'exchange'); assert.equal(capture.recorded_at_ms, Date.parse(timestamp));
    assert.equal(capture.source, exchangeSource('s', agent === 'omp' ? 'omp-entry-p' : 'p', 'hello'));
  }
});

test('apply stops cleanly at the pending limit', async t => {
  const f = fixture(t); f.file('a', [prompt(f, 'one', '1'), prompt(f, 'two', '2'), prompt(f, 'three', '3')]); const plan = await dry(f);
  f.updateBridge({ FAKE_NATIVE_PENDING_AFTER: '2' }); const first = await apply(f, plan); assert.equal(first.code, 0, first.stderr); assert.equal(first.json().outcome, 'pending_limit'); assert.equal(first.json().saved, 2);
  f.updateBridge(); const second = await apply(f, plan); assert.equal(second.code, 0, second.stderr); assert.equal(second.json().saved, 1); assert.equal(second.json().already_saved, 2);
});

test('samples are sanitized', async t => {
  const f = fixture(t); f.file('a', [prompt(f, `path ${f.project}`), prompt(f, 'glsa_' + 'A'.repeat(32), 'secret', { sessionId: 'secret' })]);
  const plan = await dry(f); assert.equal(plan.samples.length, 1); assert.equal(plan.samples[0].prompt, 'path .');
});

test('cutoff ignores future and invalid records including assistant replies', async t => {
  const cutoff = Date.now(), past = new Date(cutoff - 1000).toISOString(), future = new Date(cutoff + 1000).toISOString();
  const file = transcript(t, [user('valid', 'valid', { timestamp: past }), { ...assistant('valid reply'), timestamp: past }, { ...assistant('future reply'), timestamp: future }, user('invalid', 'invalid', { timestamp: 'invalid' }), user('future', 'future', { timestamp: future })]);
  const turns = await collect(readClaudeTurns(file, {}, cutoff)); assert.equal(turns.length, 1); assert.equal(turns[0].reply, 'valid reply');
});

test('reader and binding skips are counted', async t => {
  const f = fixture(t), ignored = join(f.project, 'ignored'); mkdirSync(ignored);
  const config = JSON.parse(readFileSync(f.config)); config.ignoreRoots = [ignored]; writeFileSync(f.config, JSON.stringify(config), { mode: 0o600 });
  const file = f.file('a', [prompt(f, 'no key', undefined, { promptId: undefined }), prompt(f, 'ignored', 'i', { cwd: ignored }), prompt(f, 'missing', 'm', { cwd: join(f.project, 'missing') })]);
  appendFileSync(file, '{bad json}\n'); const plan = await dry(f);
  assert.deepEqual(plan.skipped, { malformed_line: 1, no_turn_key: 1, ignored: 1, missing_cwd: 1 });
});

test('ten consecutive rejected results stop the import', async t => {
  const f = fixture(t); f.file('a', Array.from({ length: 11 }, (_, i) => prompt(f, 'failure', `p${i}`))); const plan = await dry(f);
  f.updateBridge({ FAKE_NATIVE_REJECT: 'failure' }); const result = await apply(f, plan);
  assert.equal(result.code, 2, result.stderr); assert.equal(result.json().outcome, 'failed'); assert.equal(result.json().rejected, 10); assert.equal(f.calls().length, 10);
});

test('a large dry-run plan is fully flushed through a pipe', async t => {
  const f = fixture(t);
  const promptText = '🟢'.repeat(2000), replyText = '🔵'.repeat(4000);
  f.file('a', Array.from({ length: 10 }, (_, i) => [prompt(f, promptText, `p${i}`), assistant(replyText)]).flat());
  const child = spawn(process.execPath, [IMPORTER, '--agent', f.agent, '--cwd', f.project], {
    env: { PATH: process.env.PATH, HOME: f.home, LAMBDA_MEMORY_CONFIG: f.config }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '', stderr = '';
  child.stdout.setEncoding('utf8');
  child.stdout.pause();
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  const closed = new Promise(done => child.once('close', done));
  await new Promise(done => setTimeout(done, 300));
  child.stdout.resume();
  assert.equal(await closed, 0, stderr);
  assert.ok(Buffer.byteLength(stdout) > 64 * 1024);
  const plan = JSON.parse(stdout);
  assert.equal(plan.samples.length, 10);
  assert.equal(plan.samples[9].prompt, promptText);
  assert.equal(plan.samples[9].reply, replyText);
});

test('a historical cwd replaced by a file is skipped as missing', async t => {
  const f = fixture(t), replaced = join(f.project, 'replaced');
  writeFileSync(replaced, 'not a directory');
  f.file('a', [prompt(f, 'kept'), prompt(f, 'replaced', 'replaced', { cwd: replaced })]);
  const plan = await dry(f);
  assert.equal(plan.turns, 1);
  assert.equal(plan.skipped.missing_cwd, 1);
});

test('fake conflict is opt-in even for missing or empty source', async () => {
  async function capture(conflict, source) {
    const env = { PATH: process.env.PATH };
    if (conflict !== undefined) env.FAKE_NATIVE_CONFLICT = conflict;
    const child = spawn(process.execPath, [NATIVE], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    const reply = new Promise((done, reject) => {
      let data = '';
      child.once('error', reject);
      child.stdout.on('data', chunk => {
        data += chunk;
        if (data.includes('\n')) { done(JSON.parse(data.split('\n')[0]).result); child.kill(); }
      });
    });
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'memory_capture', arguments: source === undefined ? {} : { source } } })}\n`);
    return reply;
  }
  for (const [conflict, source] of [[undefined, undefined], ['', ''], ['other', 'source']]) {
    const result = await capture(conflict, source);
    assert.equal(result.isError, false);
    assert.equal(JSON.parse(result.content[0].text).name, 'memory_capture');
  }
  const result = await capture('source', 'source');
  assert.equal(result.isError, true);
  assert.equal(JSON.parse(result.content[0].text).error, 'lifecycle or source conflict');
});

test('since is enforced and the same snapshot is stable', async t => {
  const f = fixture(t); f.file('a', [prompt(f, 'before'), prompt(f, 'after', 'after', { timestamp: new Date(Date.now() - 1000).toISOString() })]);
  const since = new Date(Date.now() - 5000).toISOString(); const plan = await dry(f, ['--since', since]); assert.equal(plan.turns, 1); assert.equal(plan.skipped.before_since, 1);
  const result = await apply(f, plan, ['--since', since]); assert.equal(result.code, 0, result.stderr); assert.equal(result.json().saved, 1);
});

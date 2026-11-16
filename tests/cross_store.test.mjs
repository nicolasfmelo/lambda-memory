import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { copyId, drainForwards, mergeBrief, mergeRecall, resultBody, routeTool, workspaceChildren } from '../runtime/cross-store.mjs';
import { userIdentity } from '../runtime/project-binding.mjs';
import { MAX_RESPONSE_BYTES } from '../runtime/framing.mjs';

const result = (body, isError = false) => ({ content: [{ type: 'text', text: JSON.stringify(body) }], isError });
const recall = (context, id, truncated = false) => result({ context, mode: 'tree', truncated, hits: [{ node: id }] });

test('mergeRecall orders scopes and joins project, user and workspace contexts', () => {
  const merged = resultBody(mergeRecall(recall('project', 'p'), recall('user', 'u'), [{ cwd: '/workspace/child', result: recall('child', 'c', true) }]));
  assert.deepEqual(merged.hits, [{ node: 'p', scope: 'project' }, { node: 'u', scope: 'user' }, { node: 'c', scope: 'workspace' }]);
  assert.equal(merged.context, 'project\n\nUser-level memory:\nuser\n\nWorkspace memory (child):\nchild');
  assert.equal(merged.mode, 'tree');
  assert.equal(merged.truncated, true);
});

test('mergeRecall preserves failed primary results and ignores failed sides', () => {
  const failed = result({ error: 'unavailable' }, true);
  assert.equal(mergeRecall(failed, recall('user', 'u')), failed);
  assert.equal(mergeRecall(null, recall('user', 'u')), null);
  assert.equal(mergeRecall(new Error('failed'), null).message, 'failed');
  const merged = resultBody(mergeRecall(recall('project', 'p'), failed, [{ cwd: '/c', result: null }, { cwd: '/d', result: result('invalid') }]));
  assert.equal(merged.context, 'project');
  assert.deepEqual(merged.hits, [{ node: 'p', scope: 'project' }]);
});

test('mergeRecall applies a UTF-8 byte budget in project, user, workspace order', () => {
  const primary = recall('project', 'p'), user = recall('ééé', 'u');
  const prefix = 'project\n\nUser-level memory:\n';
  const merged = resultBody(mergeRecall(primary, user, [], Buffer.byteLength(prefix) + 3));
  assert.equal(merged.context, prefix + 'é');
  assert.equal(merged.truncated, true);
  assert.ok(Buffer.byteLength(merged.context) <= Buffer.byteLength(prefix) + 3);
  assert.equal(resultBody(mergeRecall(primary, user, [], 0)).context, '');
  assert.equal(resultBody(mergeRecall(primary, user, [], 7)).context, 'project');
});

test('mergeRecall bounds escaped serialized hits and preserves the primary fallback', () => {
  const project = recall('project', '\\"'.repeat(100000));
  const side = recall('user', '\\"'.repeat(100000));
  const merged = mergeRecall(project, side, [{ cwd: '/child', result: side }]);
  assert.ok(Buffer.byteLength(JSON.stringify(merged)) <= MAX_RESPONSE_BYTES - 65536);
  assert.deepEqual(resultBody(merged).hits, [{ node: resultBody(project).hits[0].node, scope: 'project' }]);
  assert.equal(resultBody(merged).truncated, true);
  const huge = recall('p', 'x'.repeat(MAX_RESPONSE_BYTES));
  assert.equal(mergeRecall(huge, side), huge);
});

test('mergeBrief adds user roots and non-empty workspace roots only', () => {
  const primary = result({ project: 'p', mode: 'tree', roots: ['project'], truncated: false, skill_drafts_pending: 0 });
  assert.deepEqual(resultBody(mergeBrief(primary, result({ roots: ['preferences'] }), [
    { project: 'c', cwd: '/workspace/child', result: result({ roots: ['architecture'] }) },
    { project: 'empty', cwd: '/workspace/empty', result: result({ roots: [] }) },
  ])), { project: 'p', mode: 'tree', roots: ['project'], truncated: false, skill_drafts_pending: 0, user: { roots: ['preferences'] }, workspace: [{ project: 'c', name: 'child', roots: ['architecture'] }] });
  assert.deepEqual(resultBody(mergeBrief(primary, null)), resultBody(primary));
  const failed = result({ error: 'failed' }, true);
  assert.equal(mergeBrief(failed, result({ roots: ['preferences'] })), failed);
});

test('brief_counts_pending_drafts_across_stores', () => {
  const p = result({ roots: [], skill_drafts_pending: 2 }), u = result({ roots: [], skill_drafts_pending: 3 });
  assert.equal(resultBody(mergeBrief(p, u)).skill_drafts_pending, 5);
  assert.equal(resultBody(mergeBrief(p, null)).skill_drafts_pending, 2);
  assert.equal(resultBody(mergeBrief(result({ roots: [] }), u)).skill_drafts_pending, 3);
  assert.equal(resultBody(mergeBrief(p, u, [{ project: 'child', cwd: '/child', result: result({ roots: [], skill_drafts_pending: 9 }) }])).skill_drafts_pending, 5);
  for (const name of ['memory_skill_candidates', 'memory_skill_candidate', 'memory_skill_decide']) assert.throws(() => routeTool(name, {}, 'p', 'user:o'), /unknown tool/);
});

test('workspaceChildren picks four recent strictly nested canonical directories', async t => {
  const root = mkdtempSync(join(tmpdir(), 'lm-children-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const workspace = join(root, 'workspace'); mkdirSync(workspace);
  const outside = join(root, 'workspace-other'); mkdirSync(outside);
  const entries = [];
  for (let i = 0; i < 6; i++) {
    const cwd = join(workspace, `child${i}`); mkdirSync(cwd);
    entries.push({ project: `c${i}`, cwd, last_seen_ms: i });
  }
  const ignored = join(workspace, 'ignored'); mkdirSync(ignored);
  const escape = join(workspace, 'escape'); symlinkSync(outside, escape);
  entries.push({ project: 'p', cwd: workspace, last_seen_ms: 100 }, { project: 'user:owner', cwd: entries[0].cwd, last_seen_ms: 100 },
    { project: 'equal', cwd: workspace, last_seen_ms: 100 }, { project: 'outside', cwd: outside, last_seen_ms: 100 },
    { project: 'escape', cwd: escape, last_seen_ms: 100 }, { project: 'ignored', cwd: ignored, last_seen_ms: 100 },
    { project: 'missing', cwd: join(root, 'missing'), last_seen_ms: 100 });
  assert.deepEqual((await workspaceChildren({ project: 'p', cwd: workspace }, 'user:owner', entries, [ignored])).map(child => child.project), ['c5', 'c4', 'c3', 'c2']);
});

test('userIdentity hashes owners only when the UTF-8 identity exceeds 512 bytes', () => {
  assert.equal(userIdentity('owner'), 'user:owner');
  assert.equal(userIdentity('x'.repeat(507)), 'user:' + 'x'.repeat(507));
  const owner = 'é'.repeat(254);
  assert.equal(userIdentity(owner), `user:${createHash('sha256').update(owner).digest('hex')}`);
});

test('routing strips capture scope and keeps explicit user calls out of fanout', () => {
  assert.deepEqual(routeTool('memory_capture', { scope: 'user', project: 'p', content: 'text' }, 'p', 'user:o'), { target: 'user', args: { project: 'user:o', content: 'text' } });
  assert.deepEqual(routeTool('memory_capture', { scope: 'project', project: 'p' }, 'p', 'user:o'), { target: 'project', args: { project: 'p' } });
  assert.throws(() => routeTool('memory_capture', { scope: null }, 'p', 'user:o'), { rpcCode: -32602 });
  assert.throws(() => routeTool('memory_forwards', {}, 'user:o', 'user:o'), /unknown tool/);
  assert.equal(routeTool('memory_node', { project: 'user:o' }, 'p', 'user:o').target, 'user');
  assert.equal(routeTool('memory_recall', { projects: ['user:o'] }, 'p', 'user:o').target, 'user');
  assert.equal(routeTool('memory_recall', { projects: ['p', 'other'] }, 'p', 'user:o').target, 'project');
  assert.equal(routeTool('memory_recall', {}, 'user:o', 'user:o').target, 'project');
});

test('operator routes skill tools but never forwarding tools', () => {
  for (const name of ['memory_skill_candidates', 'memory_skill_candidate', 'memory_skill_decide']) {
    assert.equal(routeTool(name, { project: 'user:o' }, 'p', 'user:o', { operator: true }).target, 'user');
    assert.throws(() => routeTool(name, {}, 'p', 'user:o'), /unknown tool/);
  }
  for (const name of ['memory_forwards', 'memory_forward_done']) assert.throws(() => routeTool(name, {}, 'p', 'user:o', { operator: true }), /unknown tool/);
});

test('an explicit project capture scope takes precedence over the user project argument', () => {
  assert.deepEqual(routeTool('memory_capture', { scope: 'project', project: 'user:o', source: 's' }, 'p', 'user:o'),
    { target: 'project', args: { project: 'user:o', source: 's' } });
  assert.deepEqual(routeTool('memory_capture', { project: 'user:o', source: 's' }, 'p', 'user:o'),
    { target: 'user', args: { project: 'user:o', source: 's' } });
});

const source = { kind: 'decision', outcome: 'unknown', content: 'preference', recorded_at_ms: 123 };
const forward = (id, action = 'capture') => ({ forward_id: id, source_id: id, action, source: action === 'capture' ? source : null });
async function drain(forwards, answer, stopped) {
  const calls = [];
  const state = await drainForwards({ project: 'p', user: 'user:o', stopped,
    callProject: async (name, args) => { calls.push({ name, args }); return result({ forwards }); },
    callUser: async (name, args) => { calls.push({ name, args }); return answer(name, args); } });
  return { calls, state };
}

test('copy IDs use deterministic project hashes and bound long source IDs', () => {
  assert.equal(copyId('p', 'source'), `promote/${createHash('sha256').update('p').digest('hex').slice(0, 16)}/source`);
  const long = copyId('p', 'é'.repeat(256));
  assert.match(long, /^promote\/[a-f0-9]{16}\/h-[a-f0-9]{64}$/);
});

test('drain acknowledges successful, missing and null-source forwards in listed order', async () => {
  const { calls } = await drain([forward('capture'), forward('purge', 'purge'), { ...forward('missing'), source: null }],
    name => name === 'memory_capture' ? result({ status: 'already_saved_pending' }) : result({ error: 'record not found' }, true));
  assert.deepEqual(calls.map(call => call.name), ['memory_forwards', 'memory_capture', 'memory_purge', 'memory_forward_done']);
  assert.deepEqual(calls.at(-1).args, { project: 'p', forwards: ['capture', 'purge', 'missing'] });
  assert.equal(calls[1].args.origin, 'promote://p/capture');
  assert.equal(calls[1].args.recorded_at_ms, 123);
});

test('drain marks permanent failures and leaves transient errors pending without retry', async () => {
  let i = 0;
  const replies = [() => result({ error: 'invalid runtime input: secret' }, true),
    () => result({ error: 'lifecycle or source conflict' }, true),
    () => { throw Object.assign(new Error('rpc error'), { rpcCode: -32602 }); },
    () => result({ error: 'runtime storage unavailable' }, true), () => { throw new Error('provider failed'); }];
  const { calls } = await drain(replies.map((_, index) => forward(String(index))), () => replies[i++]());
  assert.equal(i, 5);
  assert.deepEqual(calls.at(-1).args, { project: 'p', forwards: [], failed: ['0', '1', '2'] });
});

test('an uncertain purge blocks later forwards of its source only', async () => {
  const queue = [{ ...forward('purge:S', 'purge'), source_id: 'S' },
    { ...forward('capture:S:2'), source_id: 'S' }, { ...forward('capture:T'), source_id: 'T' }];
  const { calls } = await drain(queue, name => name === 'memory_purge'
    ? result({ error: 'runtime storage unavailable' }, true) : result({ status: 'saved_pending' }));
  assert.deepEqual(calls.filter(c => c.name === 'memory_capture').map(c => c.args.source), [copyId('p', 'T')]);
  assert.deepEqual(calls.at(-1).args, { project: 'p', forwards: ['capture:T'] });
});

test('capacity stops a drain immediately and requests backoff', async () => {
  const { calls, state } = await drain([forward('first'), forward('later')], () => result({ error: 'pending capture capacity reached' }, true));
  assert.equal(state.backoff, true);
  assert.deepEqual(calls.map(call => call.name), ['memory_forwards', 'memory_capture']);
});

test('drain stops without acknowledgement when the service stops', async () => {
  let stopped = false;
  const { calls } = await drain([forward('first'), forward('later')], () => { stopped = true; return result({ status: 'saved_pending' }); }, () => stopped);
  assert.deepEqual(calls.map(call => call.name), ['memory_forwards', 'memory_capture']);
});

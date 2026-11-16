import test from 'node:test';
import assert from 'node:assert/strict';
import { approveRequest, initialSkillReview, skillReview } from '../runtime/web/lib/skill-state.mjs';

const preview = {
  candidate: 'shared-id',
  revision_id: 'revision-a',
  draft_hash: 'hash-a',
  paths: [{ path: '/first', action: 'create' }, { path: '/second', action: 'replace' }],
};

function reviewing() {
  let state = initialSkillReview('a');
  state = skillReview(state, { type: 'select', project: 'a', candidate: 'shared-id' });
  state = skillReview(state, { type: 'preview', project: 'a', candidate: 'shared-id', preview });
  return skillReview(state, { type: 'confirm', project: 'a', kind: 'approve' });
}

test('changing project clears selection, preview and confirmation', () => {
  const state = reviewing();
  assert.equal(state.confirmation.preview.project, 'a');
  assert.deepEqual(skillReview(state, { type: 'project', project: 'b' }), initialSkillReview('b'));
});

test('late preview and confirmation responses from another project are ignored', () => {
  let state = skillReview(reviewing(), { type: 'project', project: 'b' });
  state = skillReview(state, { type: 'select', project: 'b', candidate: 'shared-id' });
  const before = state;
  state = skillReview(state, { type: 'preview', project: 'a', candidate: 'shared-id', preview });
  assert.strictEqual(state, before);
  state = skillReview(state, { type: 'confirm', project: 'a', kind: 'approve' });
  assert.strictEqual(state, before);
  state = skillReview(state, { type: 'preview', project: 'b', candidate: 'other', preview });
  assert.strictEqual(state, before);
  state = skillReview(state, { type: 'status', project: 'a', candidate: 'shared-id', status: 'installed' });
  assert.strictEqual(state, before);
  state = skillReview(state, { type: 'clear', project: 'a' });
  assert.strictEqual(state, before);
});

test('approval uses the project and exact ordered values of the reviewed preview', () => {
  const view = reviewing().confirmation.preview;
  assert.deepEqual(approveRequest(view), {
    project: 'a',
    candidate: 'shared-id',
    revision: 'revision-a',
    draft_hash: 'hash-a',
    paths: ['/first', '/second'],
    confirm: true,
  });
});

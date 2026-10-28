import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  READ_ONLY_TOOLS,
  buildEpisode,
  sanitize,
  shortHash,
  sourceId,
} from '../runtime/action-episode.mjs';
import { describeOmp, describeClaudeCode } from '../runtime/hook-adapters/tool-kinds.mjs';

const HOME = '/home/u';
const ROOT = '/home/u/Projects/p';
const ctx = { home: HOME, root: ROOT };

test('sanitize removes secrets', () => {
  const hex40 = '0123456789abcdef0123456789abcdef01234567';
  const b64 = 'Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MEFCQ0RFRkdISUo=';
  assert.equal(hex40.length, 40);
  assert.equal(b64.length, 44);
  const table = [
    ['export OPENROUTER_API_KEY=sk-or-v1-abc123', 'sk-or-v1-abc123'],
    ['curl -H "Authorization: Bearer eyJhbGciOi"', 'eyJhbGciOi'],
    ['git clone https://user:ghp_AAAA1111@github.com/x/y', 'ghp_AAAA1111'],
    ['--token=xoxb-123-456', 'xoxb-123-456'],
    ['{"password": "hunter2"}', 'hunter2'],
    ['aws s3 ls # AKIAIOSFODNN7EXAMPLE', 'AKIAIOSFODNN7EXAMPLE'],
    ['key AIzaSyA1234567890', 'AIzaSyA1234567890'],
    [`checksum ${hex40}`, hex40],
    [`payload ${b64}`, b64],
    ['mysql -u root -pS3cretPass', 'S3cretPass'],
    ["mysqldump -h db -u root -p'Quoted Pass9' app", 'Quoted Pass9'],
    ['mariadb -pMar1aPass -e "select 1"', 'Mar1aPass'],
    ['/usr/bin/mariadb-dump -u root -pDumpPass9 app', 'DumpPass9'],
    ['docker login -u me -p DockerPw7 registry.example.com', 'DockerPw7'],
    ['docker login --password DockerPw8 registry.example.com', 'DockerPw8'],
    ['podman login --password=PodmanPw9 registry.example.com', 'PodmanPw9'],
    ['nerdctl login -u me -p=NerdPw10 registry.example.com', 'NerdPw10'],
    ['helm registry login -u me -p HelmPw11 registry.example.com', 'HelmPw11'],
    ['sshpass -p SshPw12 ssh me@host', 'SshPw12'],
    ["sshpass -p 'Ssh Pw 13' ssh me@host", 'Ssh Pw 13'],
    ['sshpass -p SshPw14 ssh -p 22 me@host', 'SshPw14'],
  ];
  for (const [input, secret] of table) {
    const output = sanitize(input, ctx);
    assert.ok(!output.includes(secret), `secret survived in: ${output}`);
    assert.ok(output.includes('[REDACTED]'), `no marker in: ${output}`);
  }
});

test('sanitize redacts short password flags in place', () => {
  const table = [
    ['mysql -u root -pS3cretPass', 'mysql -u root -p[REDACTED]'],
    ["mysqldump -u root -p'a b' app", 'mysqldump -u root -p[REDACTED] app'],
    ['docker login -u me -p DockerPw7 reg.io', 'docker login -u me -p [REDACTED] reg.io'],
    ['sshpass -p SshPw12 ssh -p 22 me@host', 'sshpass -p [REDACTED] ssh -p 22 me@host'],
    ['docker run -p 80:80 img login -p 1', 'docker run -p 80:80 img login -p 1'],
  ];
  for (const [input, expected] of table) assert.equal(sanitize(input, ctx), expected);
});

test('sanitize keeps ordinary commands', () => {
  for (const command of [
    'cargo test --features memory-runtime,router-http',
    'git status',
    'npm run build',
    'node --test tests/*.test.mjs',
    'mkdir -p ./a/b',
    'cargo test -p lambda-memory',
    'git -p log',
    'ssh -p 22 me@host',
    'docker run -p 8080:80 nginx',
    'docker login registry.example.com',
    'mysql -u root -p',
    'mysql -u root app',
    'sshpass -e ssh -p 22 me@host',
    'helm install -p x chart',
  ]) {
    assert.equal(sanitize(command, ctx), command);
  }
});

test('sanitize normalizes paths', () => {
  assert.equal(sanitize('/home/u/Projects/p/src/a.rs', ctx), './src/a.rs');
  assert.equal(sanitize('/home/u/.cargo/bin', ctx), '~/.cargo/bin');
  const long = 'cargo test --manifest-path /home/u/Projects/p/some/very/long/nested/directory/structure/Cargo.toml';
  assert.equal(
    sanitize(long, ctx),
    'cargo test --manifest-path ./some/very/long/nested/directory/structure/Cargo.toml',
  );
});

test('sourceId is stable and accepts pipes', () => {
  const id = sourceId('s', 'call_1|fc_2');
  assert.match(id, /^[0-9a-f]{16}\/[0-9a-f]{16}$/);
  assert.equal(id, sourceId('s', 'call_1|fc_2'));
  assert.equal(id, `${shortHash('s')}/${shortHash('call_1|fc_2')}`);
  assert.notEqual(id, sourceId('s', 'call_1|fc_3'));
});

const base = {
  outcome: 'succeeded',
  errorText: '',
  sessionId: 'session-1',
  callId: 'call_1|fc_2',
  seq: 3,
  home: HOME,
  root: ROOT,
};

test('buildEpisode shapes per tool', () => {
  const bash = buildEpisode({
    ...base,
    tool: 'bash',
    ...describeOmp('bash', { command: 'cargo test\nmore', i: 'Running tests' }),
  });
  assert.equal(bash.kind, 'action');
  assert.equal(bash.outcome, 'succeeded');
  assert.equal(bash.source, sourceId('session-1', 'call_1|fc_2'));
  const parsed = JSON.parse(bash.content);
  assert.equal(parsed.v, 1);
  assert.equal(parsed.tool, 'bash');
  assert.equal(parsed.summary, 'cargo test');
  assert.equal(parsed.intent, 'Running tests');
  assert.equal(parsed.error_head, null);
  assert.equal(parsed.session, shortHash('session-1'));
  assert.equal(parsed.seq, 3);

  const edit = buildEpisode({
    ...base,
    tool: 'edit',
    ...describeOmp('edit', { path: '/home/u/Projects/p/src/a.rs' }),
  });
  assert.equal(JSON.parse(edit.content).summary, './src/a.rs');

  const claude = buildEpisode({
    ...base,
    tool: 'Bash',
    ...describeClaudeCode('Bash', { command: 'npm run build', description: 'Building the app' }),
  });
  assert.equal(JSON.parse(claude.content).intent, 'Building the app');
  assert.equal(JSON.parse(claude.content).summary, 'npm run build');

  const task = buildEpisode({
    ...base,
    tool: 'Task',
    ...describeClaudeCode('Task', { description: 'Explore code', prompt: 'secret-ish long prompt' }),
  });
  assert.equal(JSON.parse(task.content).summary, '');

  const agentTool = buildEpisode({
    ...base,
    tool: 'Agent',
    ...describeClaudeCode('Agent', { description: 'Explore code', prompt: 'secret-ish long prompt' }),
  });
  assert.equal(agentTool.outcome, 'succeeded');
  assert.equal(JSON.parse(agentTool.content).tool, 'Agent');
  assert.equal(JSON.parse(agentTool.content).intent, 'Explore code');
  assert.equal(JSON.parse(agentTool.content).summary, '');

  assert.equal(buildEpisode({ ...base, tool: 'read', ...describeOmp('read', { path: '/x' }) }), null);

  const failed = buildEpisode({
    ...base,
    tool: 'read',
    ...describeOmp('read', { path: '/x' }),
    outcome: 'failed',
    errorText: '\n\n  ENOENT: no such file  \nsecond line',
  });
  assert.equal(failed.outcome, 'failed');
  const failedBody = JSON.parse(failed.content);
  assert.equal(failedBody.summary, '');
  assert.equal(failedBody.error_head, 'ENOENT: no such file');

  const long = buildEpisode({
    ...base,
    tool: 'read',
    ...describeOmp('read', {}),
    outcome: 'failed',
    errorText: `${'error '.repeat(100)}\nnext`,
  });
  assert.equal(JSON.parse(long.content).error_head.length, 200);
  assert.ok(READ_ONLY_TOOLS.has('read') && READ_ONLY_TOOLS.has('WebSearch'));
  assert.equal(READ_ONLY_TOOLS.size, 9);
});

test('buildEpisode bounds', () => {
  const episode = buildEpisode({
    ...base,
    tool: 'bash',
    ...describeOmp('bash', { command: 'x'.repeat(5000), i: 'y'.repeat(500) }),
  });
  assert.ok(episode.content.length <= 1024);
  const body = JSON.parse(episode.content);
  assert.ok(body.summary.length <= 200);
  assert.ok(body.intent.length <= 160);
  assert.equal(body.v, 1);

  const quotes = buildEpisode({
    ...base,
    tool: 'bash',
    ...describeOmp('bash', { command: '"'.repeat(500), i: '"'.repeat(500) }),
    outcome: 'failed',
    errorText: '"'.repeat(500),
  });
  assert.ok(Buffer.byteLength(quotes.content) <= 1024);
  assert.equal(JSON.parse(quotes.content).v, 1);

  const wide = buildEpisode({
    ...base,
    tool: 'bash',
    ...describeOmp('bash', { command: '界'.repeat(500), i: '界'.repeat(500) }),
  });
  assert.ok(Buffer.byteLength(wide.content) <= 1024);
});

test('buildEpisode redacts before truncating and drops unidentifiable calls', () => {
  const command = `${'a '.repeat(90)}export API_TOKEN=sk-live-abcdefghij`;
  const episode = buildEpisode({ ...base, tool: 'bash', ...describeOmp('bash', { command }) });
  assert.ok(!episode.content.includes('sk-live'));
  assert.ok(!episode.content.includes('abcdefghij'));
  assert.equal(buildEpisode({ ...base, tool: 'bash', ...describeOmp('bash', { command: 'ls' }), callId: '' }), null);
  assert.equal(buildEpisode({ ...base, tool: '9bad', ...describeOmp('9bad', {}), outcome: 'failed' }), null);
});


test('unknown outcome is preserved', () => {
  const episode = buildEpisode({ tool: 'Bash', kind: 'shell', summary: 'ls', intent: '', outcome: 'unknown', errorText: '', sessionId: 's', callId: 'c', seq: 0 });
  assert.equal(episode.outcome, 'unknown');
});

test('other kind is kept only on failure', () => {
  for (const outcome of ['succeeded', 'unknown']) {
    assert.equal(buildEpisode({ ...base, tool: 'Read', kind: 'other', summary: '', intent: '', outcome }), null);
  }
  assert.ok(buildEpisode({ ...base, tool: 'Read', kind: 'other', summary: '', intent: '', outcome: 'failed' }));
});

test('describeClaudeCode maps tool kinds', () => {
  for (const [tool, kind] of [['Bash', 'shell'], ['MultiEdit', 'edit'], ['Write', 'write'], ['Agent', 'task'], ['Read', 'other']]) {
    assert.equal(describeClaudeCode(tool, {}).kind, kind);
  }
  assert.deepEqual(describeClaudeCode('Bash', { command: 'ls\nmore', description: 'list' }), { kind: 'shell', summary: 'ls\nmore', intent: 'list' });
});

test('describeOmp maps tool kinds', () => {
  for (const [tool, kind] of [['bash', 'shell'], ['eval', 'shell'], ['edit', 'edit'], ['write', 'write'], ['task', 'task']]) {
    assert.equal(describeOmp(tool, { i: 'intent' }).kind, kind);
    assert.equal(describeOmp(tool, { i: 'intent' }).intent, 'intent');
  }
  assert.equal(describeOmp('eval', { code: 'x' }).summary, 'x');
  assert.equal(describeOmp('edit', { file_path: 'a' }).summary, 'a');
});

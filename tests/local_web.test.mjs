import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { request as httpRequest } from 'node:http';
import { createConnection, createServer } from 'node:net';
import { createInterface } from 'node:readline';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { serve } from '../runtime/local-service.mjs';
import { loadConfig } from '../runtime/project-binding.mjs';
import { recordProject } from '../runtime/project-registry.mjs';
import {
  allowedHost, allowedOrigin, daemonWebStarter, parseProcNetTcp, startWebService,
} from '../runtime/web-service.mjs';

const TOKEN = 'lambda-memory-web-test-token-0123456789!';
const OWNER = 'owner-secret-identity';
const ACTOR = 'actor-secret-identity';
const SERVICE = resolve('runtime/local-service.mjs');
const FAKE_NATIVE = resolve('tests/support/fake-native.cjs');
const HEADER = '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n';
const hex = (port) => port.toString(16).toUpperCase().padStart(4, '0');
const row = (local, remote, uid, state = '01') => `   0: ${local} ${remote} ${state} 00000000:00000000 00:00000000 00000000  ${uid}        0 1 1 0000000000000000 20 4 30 10 -1\n`;
const fakeProc = (uid = process.getuid()) => ({ peerPort, serverPort }) => HEADER
  + row(`0100007F:${hex(serverPort)}`, `0100007F:${hex(peerPort)}`, 0)
  + row(`0100007F:${hex(peerPort)}`, `0100007F:${hex(serverPort)}`, uid);

async function freePort() {
  const server = createServer();
  await new Promise((done) => server.listen(0, '127.0.0.1', done));
  const { port } = server.address();
  await new Promise((done) => server.close(done));
  return port;
}

function fixture(extra = {}) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'lm-web-')));
  chmodSync(FAKE_NATIVE, 0o755);
  const config = join(root, 'runtime.json');
  writeFileSync(config, JSON.stringify({
    nativeBin: FAKE_NATIVE, bridgeBin: FAKE_NATIVE, dataDir: join(root, 'data'), socketPath: join(root, 'run', 's.sock'),
    owner: OWNER, actor: ACTOR, token: TOKEN, skillTargets: { project: ['.test-skills'], user: [join(root, 'user-skills')] }, ...extra,
  }), { mode: 0o600 });
  return { root, config, data: join(root, 'data') };
}

function call(port, method, path, { headers = {}, body, host = `127.0.0.1:${port}` } = {}) {
  return new Promise((done, failed) => {
    const req = httpRequest({ host: '127.0.0.1', port, method, path, agent: false, headers: { Host: host, ...headers } }, (res) => {
      const chunks = [];
      res.on('data', (chunk) => chunks.push(chunk));
      res.on('end', () => {
        const text = Buffer.concat(chunks).toString('utf8');
        for (const secret of [TOKEN, OWNER, ACTOR]) assert.ok(!text.includes(secret), 'response leaked a secret');
        let json;
        try { json = JSON.parse(text); } catch { /* static file */ }
        done({ status: res.statusCode, headers: res.headers, text, json });
      });
    });
    req.on('error', failed);
    if (body !== undefined) req.write(typeof body === 'string' ? body : JSON.stringify(body));
    req.end();
  });
}

// Runs the daemon in-process with the web UI on an ephemeral port and the given test overrides.
async function startDaemon(t, { webLeaseMs = 120000, readProcNetTcp = fakeProc(), projects = 1, staticFixture = false, owner = OWNER, fakeUi = false, ownerPath = false } = {}) {
  const port = await freePort();
  const paths = fixture({ webPort: port, owner });
  if (fakeUi) {
    const wrapper = join(paths.root, 'native.cjs');
    paths.skillState = join(paths.root, 'skills.json'); paths.captureStore = join(paths.root, 'captures');
    writeFileSync(paths.skillState, JSON.stringify([{ candidate_id: 'user-c', memory_id: 'user-m', scope: 'user', slug: 'user-skill', status: 'drafted', revision_id: 'user-r', draft: 'user draft', install_paths: [], install_hashes: [] }]));
    writeFileSync(wrapper, `#!/usr/bin/env node\nObject.assign(process.env, ${JSON.stringify({ FAKE_NATIVE_UI: '1', FAKE_NATIVE_SKILLS: paths.skillState, FAKE_NATIVE_STORE: paths.captureStore })});\nrequire(${JSON.stringify(FAKE_NATIVE)});\n`, { mode: 0o755 });
    const config = JSON.parse(readFileSync(paths.config, 'utf8')); config.nativeBin = wrapper;
    writeFileSync(paths.config, JSON.stringify(config));
  }
  let webDir;
  if (staticFixture) {
    webDir = join(paths.root, 'web'); mkdirSync(join(webDir, 'lib'), { recursive: true }); mkdirSync(join(webDir, 'vendor'));
    writeFileSync(join(webDir, 'index.html'), '<meta name="lm-csrf" content="__LM_CSRF__"><script type="module" src="/app.js"></script>');
    writeFileSync(join(webDir, 'app.js'), ''); writeFileSync(join(webDir, 'lib', 'x.mjs'), 'export default 1');
    writeFileSync(join(webDir, 'lib', 'x.css'), 'x{}'); writeFileSync(join(webDir, 'lib', 'x.woff2'), 'font');
    writeFileSync(join(webDir, 'vendor', 'htm-preact-3.1.1.mjs'), 'export default 2'); writeFileSync(join(webDir, 'vendor', 'README.md'), 'not served');
    symlinkSync(paths.config, join(webDir, 'lib', 'secret.mjs'));
  }
  const roots = [];
  mkdirSync(paths.data, { mode: 0o700 });
  const projectParent = ownerPath ? join(paths.root, owner) : paths.root;
  if (ownerPath) mkdirSync(projectParent);
  for (let i = 0; i < projects; i += 1) {
    const cwd = realpathSync(mkdtempSync(join(projectParent, `p${i}-`)));
    roots.push({ cwd, project: `path:${cwd}` });
    assert.equal(await recordProject(paths.data, { project: `path:${cwd}`, cwd }, 1000 + i), true);
  }
  let web, seam;
  const done = serve(loadConfig({ LAMBDA_MEMORY_CONFIG: paths.config }), {
    async startWeb(context) {
      seam = context;
      web = await startWebService({ ...context, config: { ...context.config, webLeaseMs }, readProcNetTcp, ...(webDir ? { webDir } : {}) });
      return { stop: () => web.close() };
    },
  });
  for (let i = 0; i < 200 && !web; i += 1) await new Promise((ok) => setTimeout(ok, 10));
  assert.ok(web, 'web service did not start');
  t.after(async () => { process.emit('SIGTERM'); await done; rmSync(paths.root, { recursive: true, force: true }); });
  const index = await call(port, 'GET', '/');
  const csrf = /<meta name="lm-csrf" content="([0-9a-f]+)">/.exec(index.text)?.[1];
  const api = (path, body, headers = {}) => call(port, body === undefined ? 'GET' : 'POST', path, {
    body,
    headers: {
      'X-Lambda-Memory-CSRF': csrf, ...(body === undefined ? {} : { Origin: `http://127.0.0.1:${port}`, 'Content-Type': 'application/json' }), ...headers,
    },
  });
  return { port, paths, roots, web, seam, csrf, index, api, done, project: roots[0]?.project };
}

test('pure checks: peer uid from /proc/net/tcp, host and origin allowlists', () => {
  const own = fakeProc(1000)({ peerPort: 40000, serverPort: 8765 });
  assert.equal(parseProcNetTcp(own, 40000, 8765), 1000);
  assert.equal(parseProcNetTcp(fakeProc(1234)({ peerPort: 40000, serverPort: 8765 }), 40000, 8765), 1234);
  assert.equal(parseProcNetTcp(own, 40001, 8765), null);
  assert.equal(parseProcNetTcp(HEADER + row('0100007F:9C40', '0100007F:223D', 0), 8765, 40000), null);
  assert.equal(parseProcNetTcp('', 1, 2), null);
  const stale = row('0100007F:9C40', '0100007F:223D', 0, '06');
  assert.equal(parseProcNetTcp(HEADER + stale + row('0100007F:9C40', '0100007F:223D', 1000), 40000, 8765), 1000, 'TIME_WAIT row skipped');
  assert.equal(parseProcNetTcp(HEADER + stale, 40000, 8765), null, 'only established rows count');
  assert.equal(allowedHost('127.0.0.1:8765', 8765), true);
  assert.equal(allowedHost('localhost:8765', 8765), true);
  for (const bad of [undefined, '', '127.0.0.1', '127.0.0.1:8766', 'evil.test:8765', 'LOCALHOST:8765', '127.0.0.1:8765.evil', '[::1]:8765']) assert.equal(allowedHost(bad, 8765), false, String(bad));
  assert.equal(allowedOrigin('http://127.0.0.1:8765', 8765), true);
  assert.equal(allowedOrigin('http://localhost:8765', 8765), true);
  for (const bad of [undefined, 'null', 'https://127.0.0.1:8765', 'http://127.0.0.1:8766', 'http://evil.test', 'http://localhost:8765/']) assert.equal(allowedOrigin(bad, 8765), false, String(bad));
});

test('the UI does not start without webPort or without a readable /proc/net/tcp', async () => {
  assert.equal(daemonWebStarter({ webPort: undefined }), undefined);
  const port = await freePort();
  await assert.rejects(startWebService({
    config: { webPort: port, webLeaseMs: 5000, dataDir: '/nonexistent' }, acquire() {}, release() {}, workers: new Map(),
    readProcNetTcp() { throw new Error('ENOENT'); },
  }));
  const logged = [];
  const original = console.error;
  console.error = (line) => logged.push(line);
  try {
    const web = await daemonWebStarter({ webPort: port }, { readProcNetTcp() { throw new Error('no proc'); } })({
      config: { webPort: port, webLeaseMs: 5000, dataDir: '/nonexistent', token: TOKEN }, acquire() {}, release() {}, workers: new Map(),
    });
    await web.stop();
  } finally { console.error = original; }
  assert.equal(logged.length, 1);
  assert.ok(!logged[0].includes(TOKEN));
  await assert.rejects(call(port, 'GET', '/'), /ECONNREFUSED/);
});

test('static index carries CSP, security headers and the CSRF meta tag, bound to loopback only', async (t) => {
  const { web, index, csrf, port } = await startDaemon(t);
  assert.equal(web.server.address().address, '127.0.0.1');
  assert.equal(index.status, 200);
  assert.match(index.headers['content-type'], /^text\/html/);
  assert.equal(index.headers['content-security-policy'], "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'");
  assert.equal(index.headers['x-content-type-options'], 'nosniff');
  assert.equal(index.headers['referrer-policy'], 'no-referrer');
  assert.equal(index.headers['cache-control'], 'no-store');
  assert.equal(index.headers['cross-origin-opener-policy'], 'same-origin');
  assert.equal(index.headers['cross-origin-resource-policy'], 'same-origin');
  assert.equal(index.headers['access-control-allow-origin'], undefined);
  assert.match(csrf, /^[0-9a-f]{64}$/);
  assert.ok(!index.text.includes('__LM_CSRF__'));
  assert.ok(!/<script(?![^>]*\bsrc=)[^>]*>/i.test(index.text), 'inline script');
  assert.equal((await call(port, 'GET', '/app.js')).status, 200);
  assert.match((await call(port, 'GET', '/app.js')).headers['content-type'], /^text\/javascript/);
  const font = await call(port, 'GET', '/fonts/fira-code-latin.woff2');
  assert.equal(font.status, 200);
  assert.match(font.headers['content-type'], /^font\/woff2/);
  assert.match(font.headers['content-security-policy'], /font-src 'self'/);
  assert.equal((await call(port, 'GET', '/index.html')).status, 404);
  assert.equal((await call(port, 'GET', '/../package.json')).status, 404);
  assert.equal((await call(port, 'POST', '/')).status, 405);
});

test('static UI serves self-hosted files with security headers', async (t) => {
  const { port, index } = await startDaemon(t);
  assert.ok(!index.text.includes('__LM_CSRF__'));
  assert.match(index.text, /<script\s+type="module"\s+src="\/app\.js"/);
  const response = await call(port, 'GET', '/app.js');
  assert.equal(response.status, 200);
  assert.match(response.headers['content-type'], /^text\/javascript/);
  assert.equal(response.headers['content-security-policy'], index.headers['content-security-policy']);
});

test('static UI: no inline script, style or handlers, no unsafe DOM sinks, no external resources', () => {
  // Content-agnostic: applies to whatever the UI ships under runtime/web/, except the vendored library.
  const root = resolve('runtime/web');
  const files = [];
  const walk = (dir) => { for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name.startsWith('.')) continue;
    const path = join(dir, entry.name);
    if (entry.isDirectory()) { if (entry.name !== 'vendor') walk(path); }
    else files.push(path);
  } };
  walk(root);
  const html = readFileSync(join(root, 'index.html'), 'utf8');
  assert.ok(html.includes('<meta name="lm-csrf" content="__LM_CSRF__">'), 'csrf placeholder');
  assert.ok(!/<script\b[^>]*>[^<]+<\/script>/i.test(html), 'inline script content');
  assert.equal((html.match(/<script\b/gi) ?? []).length, 1, 'only the app module');
  assert.ok(!/\sstyle\s*=/i.test(html), 'style= attribute');
  assert.ok(!/<style\b/i.test(html), 'inline <style>');
  assert.ok(!/\son[a-z]+\s*=/i.test(html), 'inline event handler');
  for (const file of files) {
    const text = readFileSync(file, 'utf8');
    const name = file.slice(root.length + 1);
    if (/\.(html|css|m?js)$/.test(name)) assert.ok(!/(?:https?:|['"(]\/\/)/i.test(text), `${name} references an external resource`);
    if (name.endsWith('.css')) {
      assert.ok(!/@import/i.test(text), `${name} imports something`);
      // The only resources CSS may load are the self-hosted public fonts, and each one exists.
      for (const [, url] of text.matchAll(/url\(([^)]*)\)/gi)) {
        assert.match(url, /^\/fonts\/[a-z0-9-]+\.woff2$/, `${name} url(${url})`);
        assert.ok(readFileSync(join(root, url)).length > 0, url);
      }
    }
    if (/\.m?js$/.test(name)) {
      for (const sink of [/\.innerHTML\b/, /\.outerHTML\b/, /insertAdjacentHTML/, /document\.write/, /\beval\s*\(/, /new\s+Function\b/, /dangerouslySetInnerHTML/]) {
        assert.ok(!sink.test(text), `${name} uses ${sink}`);
      }
    }
  }
});

test('host, origin, fetch-site and CSRF failures are 403; bodies must be small JSON', async (t) => {
  const { port, api, project, csrf } = await startDaemon(t);
  assert.equal((await call(port, 'GET', '/', { host: `evil.test:${port}` })).status, 403);
  assert.equal((await call(port, 'GET', '/', { host: `127.0.0.1:${port + 1}` })).status, 403);
  assert.equal((await call(port, 'GET', '/', { headers: { 'Sec-Fetch-Site': 'cross-site' } })).status, 403);
  assert.equal((await call(port, 'GET', '/', { headers: { 'Sec-Fetch-Site': 'none' } })).status, 200);
  assert.equal((await call(port, 'GET', '/', { headers: { Origin: 'http://evil.test' } })).status, 403);
  assert.equal((await call(port, 'GET', '/api/projects')).status, 403);
  assert.equal((await api('/api/projects', undefined, { 'X-Lambda-Memory-CSRF': 'f'.repeat(64) })).status, 403);
  assert.equal((await api('/api/projects', undefined, { 'X-Lambda-Memory-CSRF': 'short' })).status, 403);
  assert.equal((await api('/api/projects')).status, 200);
  assert.equal((await api('/api/start', { project }, { Origin: 'http://evil.test' })).status, 403);
  assert.equal((await api('/api/start', { project }, { Origin: `http://localhost:${port}` })).status, 403, 'origin must match host');
  const noOrigin = await call(port, 'POST', '/api/start', { body: { project }, headers: { 'X-Lambda-Memory-CSRF': csrf, 'Content-Type': 'application/json' } });
  assert.equal(noOrigin.status, 403);
  assert.equal((await api('/api/start', JSON.stringify({ project }), { 'Content-Type': 'text/plain' })).status, 415);
  assert.equal((await api('/api/start', JSON.stringify({ project, pad: 'x'.repeat(70 * 1024) }))).status, 413);
  assert.equal((await api('/api/start', '{not json')).status, 400);
  assert.equal((await api('/api/start', [project])).status, 400);
  assert.equal((await api('/api/projects', {})).status, 405);
  assert.equal((await api('/api/nothing', {})).status, 404);
  assert.equal((await call(port, 'OPTIONS', '/api/tool', { headers: { Origin: `http://127.0.0.1:${port}`, 'Access-Control-Request-Method': 'POST' } })).status, 405);
  assert.equal((await call(port, 'OPTIONS', '/api/projects')).status, 405);
});

test('a peer owned by another uid is refused', async (t) => {
  const { port } = await startDaemon(t, { readProcNetTcp: fakeProc(process.getuid() + 1) });
  assert.equal((await call(port, 'GET', '/')).status, 403);
});

test('projects, needs_start, start, forced project, purge and correct route through the worker', async (t) => {
  const { api, project, seam } = await startDaemon(t);
  const listed = await api('/api/projects');
  assert.equal(listed.json.projects.length, 2);
  assert.deepEqual(listed.json.projects[0], { project: '@user', label: 'You', live: false });
  assert.deepEqual(Object.keys(listed.json.projects[1]).sort(), ['cwd', 'last_seen_ms', 'live', 'project']);
  assert.equal(listed.json.projects[1].project, project);
  assert.equal(listed.json.projects[1].live, false);

  const tool = { project, name: 'memory_status', arguments: {} };
  const early = await api('/api/tool', tool);
  assert.equal(early.status, 409);
  assert.equal(early.json.needs_start, true);
  assert.equal(seam.workers.size, 0, 'a tool call must not start a worker');
  assert.equal((await api('/api/purge', { project, sources: ['s1'], confirm: 's1' })).json.needs_start, true);
  assert.equal((await api('/api/start', { project: 'path:/unknown' })).status, 404);

  const started = await api('/api/start', { project });
  assert.equal(started.status, 200);
  assert.equal((await api('/api/projects')).json.projects[1].live, true);

  const forwarded = await api('/api/tool', { ...tool, name: 'memory_timeline_write', arguments: { title: 't' } });
  assert.equal(forwarded.status, 200);
  assert.deepEqual(forwarded.json, { isError: false, body: { name: 'memory_timeline_write', arguments: { title: 't', project } } });
  const same = await api('/api/tool', { ...tool, arguments: { project } });
  assert.equal(same.json.body.arguments.project, project);

  assert.equal((await api('/api/tool', { ...tool, arguments: { owner: 'x' } })).status, 400);
  assert.equal((await api('/api/tool', { ...tool, arguments: { actor: 'x' } })).status, 400);
  assert.equal((await api('/api/tool', { ...tool, arguments: { project: 'path:/other' } })).status, 400);
  assert.equal((await api('/api/tool', { ...tool, name: 'memory_recall' })).status, 400);
  assert.equal((await api('/api/tool', { ...tool, name: 'memory_purge' })).status, 400);
  assert.equal((await api('/api/tool', { ...tool, name: 'memory_capture' })).status, 400);
  assert.equal((await api('/api/tool', { ...tool, arguments: [] })).status, 400);
  assert.equal((await api('/api/tool', { ...tool, extra: 1 })).status, 400);

  assert.equal((await api('/api/purge', { project, sources: ['s1'], confirm: 's2' })).status, 400);
  assert.equal((await api('/api/purge', { project, sources: ['s1', 's2'], confirm: 's1' })).status, 400);
  assert.equal((await api('/api/purge', { project, sources: [], confirm: '' })).status, 400);
  const purged = await api('/api/purge', { project, sources: ['s1'], confirm: 's1' });
  assert.deepEqual(purged.json, { isError: false, body: { name: 'memory_purge', arguments: { project, sources: ['s1'] } } });

  const [worker] = seam.workers.values();
  const calls = [];
  const original = worker.client.callTool.bind(worker.client);
  worker.client.callTool = (name, args, options) => { calls.push([name, options.mutating]); return original(name, args, options); };
  const corrected = await api('/api/correct', { project, memory: 'm1', predecessor: 'r1', content: 'new text' });
  assert.equal(corrected.status, 200);
  const capture = corrected.json.capture.body.arguments, propose = corrected.json.propose.body.arguments;
  assert.equal(corrected.json.capture.body.name, 'memory_capture');
  assert.equal(corrected.json.propose.body.name, 'memory_propose');
  assert.match(capture.source, /^ui\/[0-9a-f-]{36}$/);
  assert.deepEqual(capture, { project, source: capture.source, kind: 'decision', outcome: 'unknown', origin: 'ui://manual-curation', content: 'new text' });
  assert.deepEqual(propose, { project, memory: 'm1', revision: `ui-${capture.source.slice(3)}`, sources: [capture.source], content: 'new text', predecessor: 'r1' });
  assert.deepEqual(calls, [['memory_capture', true], ['memory_propose', true]]);
  assert.equal((await api('/api/correct', { project, memory: 'm1', predecessor: 'r1' })).status, 400);

  // A client nonce pins the source and revision IDs, so a retry replays instead of duplicating.
  const nonce = '3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b';
  const replays = [];
  for (let i = 0; i < 2; i += 1) {
    const response = await api('/api/correct', { project, memory: 'm1', predecessor: 'r1', content: 'same', nonce });
    assert.equal(response.status, 200);
    replays.push([response.json.capture.body.arguments.source, response.json.propose.body.arguments.revision]);
  }
  assert.deepEqual(replays, [[`ui/${nonce}`, `ui-${nonce}`], [`ui/${nonce}`, `ui-${nonce}`]]);
  for (const bad of ['not-a-uuid', '3F2B8C1E-9A4D-4E6F-8B7A-1C2D3E4F5A6B', '3f2b8c1e-9a4d-1e6f-8b7a-1c2d3e4f5a6b', 7, '../x']) {
    assert.equal((await api('/api/correct', { project, memory: 'm1', predecessor: 'r1', content: 'same', nonce: bad })).status, 400, String(bad));
  }
  assert.equal((await api('/api/correct', { project, memory: 'm1', predecessor: 'r1', content: 'same', extra: 1 })).status, 400, 'strict keys');
  // Memory and predecessor follow the identity rules and are checked before anything is captured.
  calls.length = 0;
  for (const [memory, predecessor] of [[' ', 'r1'], ['m1', '\t'], ['m\u0007', 'r1'], ['m1', 'r\u0085'], ['m'.repeat(513), 'r1'], ['m1', 'é'.repeat(257)], [7, 'r1']]) {
    assert.equal((await api('/api/correct', { project, memory, predecessor, content: 'same' })).status, 400, JSON.stringify([memory, predecessor]));
  }
  assert.deepEqual(calls, [], 'invalid identities never reach the worker');
  assert.equal((await api('/api/correct', { project, memory: 'm'.repeat(512), predecessor: 'r1', content: 'same' })).status, 200, '512 bytes is allowed');

  // A propose that fails after a confirmed capture is 502 and still returns the capture receipt.
  worker.client.callTool = (name, args, options) => {
    if (name === 'memory_propose') return Promise.reject(new Error('mutating request indeterminate'));
    return original(name, args, options);
  };
  const halfway = await api('/api/correct', { project, memory: 'm1', predecessor: 'r1', content: 'half', nonce });
  assert.equal(halfway.status, 502);
  assert.equal(halfway.json.retry, false);
  assert.equal(halfway.json.capture_saved, true);
  assert.equal(halfway.json.capture.isError, false);
  assert.equal(halfway.json.capture.body.arguments.source, `ui/${nonce}`);

  // A JSON-RPC -32602 from the native worker is a definitive rejection (400); any other RPC error,
  // e.g. -32603 after the tool ran, stays unconfirmed (502).
  worker.client.callTool = original;
  const rejected = await api('/api/tool', { project, name: 'memory_timeline_write', arguments: { rpc_error_code: -32602 } });
  assert.equal(rejected.status, 400);
  assert.deepEqual(rejected.json, { error: 'invalid arguments' });
  const oversized = await api('/api/tool', { project, name: 'memory_timeline_write', arguments: { rpc_error_code: -32603 } });
  assert.equal(oversized.status, 502);
  assert.deepEqual(oversized.json, { retry: false, message: 'operation not confirmed; do not automatically retry writes' });

  worker.client.callTool = async () => ({ content: [{ type: 'text', text: '{"error":"bad"}' }], isError: true });
  const failedCapture = await api('/api/correct', { project, memory: 'm1', predecessor: 'r1', content: 'x' });
  assert.deepEqual(failedCapture.json, { capture: { isError: true, body: { error: 'bad' } } });

  worker.client.callTool = async () => { throw new Error('pending bound reached'); };
  const busy = await api('/api/tool', tool);
  assert.equal(busy.status, 503);
  assert.equal(busy.json.retry, true);
  worker.client.callTool = async () => { throw new Error('mutating request indeterminate'); };
  const unconfirmed = await api('/api/tool', tool);
  assert.equal(unconfirmed.status, 502);
  assert.deepEqual(unconfirmed.json, { retry: false, message: 'operation not confirmed; do not automatically retry writes' });
});

test('a binding that no longer resolves to the project is 409', async (t) => {
  const { api, paths, roots } = await startDaemon(t);
  await recordProject(paths.data, { project: 'path:/moved-away', cwd: roots[0].cwd }, 5000);
  const moved = await api('/api/start', { project: 'path:/moved-away' });
  assert.equal(moved.status, 409);
  assert.equal(moved.json.needs_start, undefined);
});

test('at most four UI leases are held', async (t) => {
  const { api, roots } = await startDaemon(t, { projects: 5 });
  for (const { project } of roots.slice(0, 4)) assert.equal((await api('/api/start', { project })).status, 200);
  assert.equal((await api('/api/start', { project: roots[0].project })).status, 200, 'renewal is not a new lease');
  assert.equal((await api('/api/start', { project: roots[4].project })).status, 429);
});

test('the UI lease expires after webLeaseMs without calls', async (t) => {
  const { api, project, seam } = await startDaemon(t, { webLeaseMs: 300 });
  assert.equal((await api('/api/start', { project })).status, 200);
  await new Promise((ok) => setTimeout(ok, 150));
  assert.equal((await api('/api/tool', { project, name: 'memory_status', arguments: {} })).status, 200, 'renews');
  await new Promise((ok) => setTimeout(ok, 200));
  assert.equal((await api('/api/projects')).json.projects[1].live, true, 'renewed lease still held');
  for (let i = 0; i < 100 && (await api('/api/projects')).json.projects[1].live; i += 1) await new Promise((ok) => setTimeout(ok, 20));
  assert.equal((await api('/api/projects')).json.projects[1].live, false);
  assert.equal((await api('/api/tool', { project, name: 'memory_status', arguments: {} })).json.needs_start, true);
  for (let i = 0; i < 150 && seam.workers.size; i += 1) await new Promise((ok) => setTimeout(ok, 20));
  assert.equal(seam.workers.size, 0);
});

test('user-store lease expires after a call and frees its slot', async t => {
  const { api, seam, roots } = await startDaemon(t, { webLeaseMs: 400, projects: 4 });
  assert.equal((await api('/api/start', { project: '@user' })).status, 200);
  assert.equal((await api('/api/tool', { project: '@user', name: 'memory_status' })).status, 200);
  await new Promise(done => setTimeout(done, 500));
  assert.equal((await api('/api/projects')).json.projects[0].live, false);
  for (let i = 0; i < 150 && seam.workers.has(`user:${OWNER}`); i++) await new Promise(done => setTimeout(done, 20));
  assert.equal(seam.workers.has(`user:${OWNER}`), false);
  for (const { project } of roots) assert.equal((await api('/api/start', { project })).status, 200);
});

test('the entry point serves the UI only with webPort, and SIGTERM closes HTTP', async (t) => {
  const run = async (extra) => {
    const paths = fixture(extra);
    const child = spawn(process.execPath, [SERVICE], { env: { ...process.env, LAMBDA_MEMORY_CONFIG: paths.config, NOTIFY_SOCKET: '' }, stdio: ['ignore', 'pipe', 'pipe'] });
    t.after(() => rmSync(paths.root, { recursive: true, force: true }));
    let output = '';
    await new Promise((ready, failed) => {
      child.stdout.on('data', (chunk) => { output += chunk; if (output.includes('service ready')) ready(); });
      child.once('exit', (code) => failed(new Error(`service exited ${code}`)));
    });
    return { child, output: () => output };
  };
  const plain = await run({});
  assert.ok(!plain.output().includes('web UI'));
  plain.child.kill('SIGTERM');
  await new Promise((done) => plain.child.once('exit', done));

  const port = await freePort();
  const web = await run({ webPort: port });
  assert.match(web.output(), new RegExp(`web UI listening on http://127\\.0\\.0\\.1:${port}`));
  assert.equal((await call(port, 'GET', '/')).status, 200);
  web.child.kill('SIGTERM');
  await new Promise((done) => web.child.once('exit', done));
  await assert.rejects(call(port, 'GET', '/'), /ECONNREFUSED/);
});

test('the bare owner in a project path is not rewritten', async t => {
  const { api, roots } = await startDaemon(t, { owner: 'directory-owner', ownerPath: true });
  const listed = await api('/api/projects');
  assert.equal(listed.json.projects[1].cwd, roots[0].cwd);
  assert.ok(roots[0].cwd.includes('/directory-owner/'));
});

test('nested static files are loaded once; unsupported and symlinked files are refused', async t => {
  const { port, index, paths } = await startDaemon(t, { staticFixture: true });
  for (const [path, type] of [['/lib/x.mjs', 'text/javascript'], ['/vendor/htm-preact-3.1.1.mjs', 'text/javascript'], ['/lib/x.css', 'text/css'], ['/lib/x.woff2', 'font/woff2']]) {
    const response = await call(port, 'GET', path);
    assert.equal(response.status, 200); assert.ok(response.headers['content-type'].startsWith(type));
    for (const key of ['content-security-policy', 'x-content-type-options', 'referrer-policy', 'cache-control', 'cross-origin-opener-policy', 'cross-origin-resource-policy']) assert.equal(response.headers[key], index.headers[key]);
  }
  for (const path of ['/vendor/README.md', '/../runtime.json', '/lib/secret.mjs', '/index.html', '/missing.mjs']) {
    const response = await call(port, 'GET', path); assert.equal(response.status, 404); assert.equal(response.headers['content-security-policy'], index.headers['content-security-policy']);
  }
  writeFileSync(join(paths.root, 'web/lib/x.mjs'), 'changed');
  assert.equal((await call(port, 'GET', '/lib/x.mjs')).text, 'export default 1');
});

test('@user routing, correction replay and UI skill approval', async t => {
  const { api, seam, project, paths } = await startDaemon(t, { fakeUi: true });
  assert.equal((await api('/api/start', { project })).status, 200);
  assert.equal((await api('/api/start', { project: '@user' })).status, 200);
  const user = seam.workers.get(`user:${OWNER}`);
  assert.ok(user);
  const result = await api('/api/tool', { project: '@user', name: 'memory_status' });
  assert.equal(result.json.body.arguments.project, '@user');
  const userOverview = await api('/api/tool', { project: '@user', name: 'memory_overview' });
  assert.equal(userOverview.json.body.origin, 'worker://@user');
  const userView = (await api('/api/skill/preview', { project: '@user', candidate: 'user-c' })).json;
  const userApprove = await api('/api/skill/approve', { project: '@user', candidate: 'user-c', revision: userView.revision_id, draft_hash: userView.draft_hash, paths: userView.paths.map(p => p.path), confirm: true });
  assert.equal(userApprove.status, 200); assert.equal(readFileSync(join(paths.root, 'user-skills/user-skill/SKILL.md'), 'utf8'), 'user draft');
  const replayNonce = '0f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b';
  for (let i = 0; i < 2; i++) assert.equal((await api('/api/correction', { project: '@user', node: 'user-m', content: 'corrected', nonce: replayNonce, title: 'Title', path: ['Preferences'] })).status, 200);
  assert.deepEqual(readFileSync(paths.captureStore, 'utf8').trim().split('\n'), [`ui/correction/${replayNonce}`]);
  const socket = createConnection(seam.config.socketPath);
  const lines = createInterface({ input: socket })[Symbol.asyncIterator]();
  const send = value => socket.write(`${JSON.stringify(value)}\n`);
  const next = async () => JSON.parse((await lines.next()).value);
  send({ cwd: seam.workers.get(project) ? project.slice(5) : '', token: TOKEN }); assert.equal((await next()).ready, true);
  send({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'test', version: '1' }, _meta: { lambdaMemoryToken: TOKEN } } }); await next();
  send({ jsonrpc: '2.0', method: 'notifications/initialized' });
  send({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
  const names = (await next()).result.tools.map(tool => tool.name);
  assert.ok(!names.includes('memory_overview') && !names.includes('memory_nodes'));
  socket.end();
  assert.equal((await api('/api/tool', { project, name: 'memory_skill_decide' })).status, 400);
  const worker = seam.workers.get(project), original = worker.client.callTool.bind(worker.client);
  let row = { candidate_id: 'c', memory_id: 'm', scope: 'project', slug: 'example', status: 'drafted', revision_id: 'r', draft: 'draft', install_paths: [], install_hashes: [] };
  const captures = new Set(), calls = [];
  const response = body => ({ content: [{ type: 'text', text: JSON.stringify(body) }], isError: false });
  let failDecision = false, staleOnRecheck = false, reads = 0;
  worker.client.callTool = async (name, args, options) => {
    calls.push({ name, args });
    if (name === 'memory_node') return response({ title: 'Title', path: ['Decisions', 'Topic'] });
    if (name === 'memory_overview' || name === 'memory_nodes') return response({ project: args.project });
    if (name === 'memory_capture') { captures.add(args.source); return response(args); }
    if (name === 'memory_skill_candidate') { reads += 1; return response(staleOnRecheck && reads % 2 === 0 ? { ...row, status: 'stale' } : row); }
    if (name === 'memory_skill_candidates') return response({ candidates: [row] });
    if (name === 'memory_skill_decide') {
      if (failDecision) throw new Error('failure');
      row = { ...row, status: args.decision, ...(args.install_paths ? { install_paths: args.install_paths, install_hashes: args.install_hashes } : {}) }; return response(row);
    }
    return original(name, args, options);
  };
  for (const name of ['memory_node', 'memory_overview', 'memory_nodes']) assert.equal((await api('/api/tool', { project, name })).status, 200);
  const nonce = '3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b';
  for (let i = 0; i < 2; i++) assert.equal((await api('/api/correction', { project, node: 'm', content: 'corrected', nonce, title: 'Title', path: ['Decisions', 'Topic'] })).status, 200);
  assert.equal(captures.size, 1);
  assert.equal(calls.filter(c => c.name === 'memory_capture').at(-1).args.content, 'Correction for memory "Title" (Decisions › Topic; node m):\n\ncorrected');
  const shownNonce = '5a6b7c8d-1e2f-4a3b-8c9d-0e1f2a3b4c5d';
  assert.equal((await api('/api/correction', { project, node: 'm', content: 'corrected', nonce: shownNonce, title: 'Old title', path: ['Decisions', 'Old topic'] })).status, 200);
  assert.equal(calls.filter(c => c.name === 'memory_capture').at(-1).args.content, 'Correction for memory "Old title" (Decisions › Old topic; node m):\n\ncorrected');
  assert.equal((await api('/api/correction', { project, node: 'm', content: 'x', title: 7 })).status, 400);
  assert.equal((await api('/api/correction', { project, node: 'm', content: 'x', nonce: shownNonce })).status, 400);
  assert.equal((await api('/api/skill/list', { project })).json.candidates.length, 1);
  const getPreview = async () => (await api('/api/skill/preview', { project, candidate: 'c' })).json;
  let view = await getPreview(); const path = view.paths[0].path;
  const request = () => ({ project, candidate: 'c', revision: view.revision_id, draft_hash: view.draft_hash, paths: view.paths.map(p => p.path), confirm: true });
  assert.equal((await api('/api/skill/approve', { ...request(), confirm: false })).status, 400);
  for (const patch of [{ revision: 'wrong' }, { draft_hash: '0'.repeat(16) }, { paths: [] }]) { const res = await api('/api/skill/approve', { ...request(), ...patch }); assert.equal(res.status, 409); assert.equal(res.json.changed, true); assert.equal(existsSync(path), false); }
  row.status = 'rejected'; assert.equal((await api('/api/skill/approve', request())).status, 409); row.status = 'drafted';
  mkdirSync(join(path, '..'), { recursive: true }); writeFileSync(path, 'foreign');
  view = await getPreview(); assert.equal(view.paths[0].reason, 'foreign file'); assert.equal((await api('/api/skill/approve', request())).status, 409);
  rmSync(path); view = await getPreview();
  staleOnRecheck = true; reads = 0;
  assert.equal((await api('/api/skill/approve', request())).status, 409); assert.equal(existsSync(path), false);
  staleOnRecheck = false;
  const approved = await api('/api/skill/approve', request()); assert.equal(approved.status, 200); assert.equal(readFileSync(path, 'utf8'), 'draft');
  assert.deepEqual(row.install_paths, [path]); assert.deepEqual(row.install_hashes, [createHash('sha256').update('draft').digest('hex')]);
  row = { ...row, status: 'drafted', draft: 'new draft' }; view = await getPreview(); assert.equal(view.paths[0].action, 'replace');
  const replacement = await api('/api/skill/approve', request()); assert.equal(replacement.status, 200); assert.equal(readFileSync(replacement.json.backups[0], 'utf8'), 'draft');
  row = { ...row, status: 'drafted', draft: 'third draft' }; writeFileSync(path, 'edited'); view = await getPreview(); assert.equal(view.paths[0].reason, 'edited since install'); assert.equal((await api('/api/skill/approve', request())).status, 409);
  rmSync(path); view = await getPreview(); failDecision = true;
  const failed = await api('/api/skill/approve', request()); assert.equal(failed.status, 502); assert.deepEqual(failed.json.written, [path]);
  failDecision = false; assert.equal((await api('/api/skill/reject', { project, candidate: 'c', revision: 'r' })).status, 200); assert.equal(row.status, 'rejected');
  row.status = 'drafted'; rmSync(join(path, '..'), { recursive: true });
  const outside = join(paths.root, 'outside'); mkdirSync(outside); symlinkSync(outside, join(path, '..'));
  view = await getPreview(); assert.equal(view.paths[0].reason, 'symlinked path'); assert.equal((await api('/api/skill/approve', request())).status, 409); assert.equal(existsSync(join(outside, 'SKILL.md')), false);
});

test('shutdown lets an in-flight call settle before its worker is disposed', async (t) => {
  const { api, project, seam, done } = await startDaemon(t);
  assert.equal((await api('/api/start', { project })).status, 200);
  const [worker] = seam.workers.values();
  const events = [];
  const call = worker.client.callTool.bind(worker.client), dispose = worker.client.dispose.bind(worker.client);
  worker.client.callTool = (...args) => call(...args).then((result) => { events.push('settled'); return result; }, (failure) => { events.push('rejected'); throw failure; });
  worker.client.dispose = () => { events.push('dispose'); return dispose(); };
  const slow = api('/api/tool', { project, name: 'memory_timeline_write', arguments: { delay_ms: 400 } });
  await new Promise((ok) => setTimeout(ok, 100));
  process.emit('SIGTERM');
  const response = await slow;
  assert.equal(response.status, 200);
  assert.equal(response.json.body.arguments.delay_ms, 400);
  await done;
  assert.equal(events[0], 'settled');
  assert.ok(events.includes('dispose'));
});

import assert from 'node:assert/strict';
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { clientEnvironment, nativeEnvironment } from '../runtime/project-binding.mjs';

function providerFile(t, text) {
  const root = mkdtempSync(join(tmpdir(), 'lm-env-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const providerEnvFile = join(root, 'openrouter.env');
  writeFileSync(providerEnvFile, text);
  chmodSync(providerEnvFile, 0o600);
  return { mode: 'openrouter', providerEnvFile, runtimeEnv: {} };
}

test('router base URL is optional in the provider env file', (t) => {
  const env = nativeEnvironment(providerFile(t, 'OPEN_ROUTER_KEY=token-1\n'));
  assert.equal(env.OPEN_ROUTER_KEY, 'token-1');
  assert.equal(Object.hasOwn(env, 'LAMBDA_MEMORY_ROUTER_BASE_URL'), false);
});

test('router base URL is accepted from the provider env file and forwarded to the native process only', (t) => {
  const config = providerFile(t, '# proxy\nOPEN_ROUTER_KEY=token-1\nLAMBDA_MEMORY_ROUTER_BASE_URL=https://proxy.example.com:8443\n');
  const env = nativeEnvironment(config);
  assert.equal(env.LAMBDA_MEMORY_ROUTER_BASE_URL, 'https://proxy.example.com:8443');
  const client = clientEnvironment({ ...config, configPath: '/c', bridgeBin: '/b', owner: 'o', actor: 'a', token: 't' }, { project: 'p', cwd: '/w' }, {});
  assert.equal(JSON.stringify(client).includes('proxy.example.com'), false);
  assert.equal(JSON.stringify(client).includes('token-1'), false);
});

test('provider env file still rejects duplicates, unknown names and a missing key', (t) => {
  const base = 'LAMBDA_MEMORY_ROUTER_BASE_URL=https://a.example.com\n';
  assert.throws(() => nativeEnvironment(providerFile(t, `OPEN_ROUTER_KEY=k\n${base}${base}`)), /invalid private provider environment/);
  assert.throws(() => nativeEnvironment(providerFile(t, `OPEN_ROUTER_KEY=k\nLAMBDA_MEMORY_ROUTER_URL=https://a.example.com\n`)), /invalid private provider environment/);
  assert.throws(() => nativeEnvironment(providerFile(t, base)), /provider key required/);
  assert.throws(() => nativeEnvironment(providerFile(t, 'OPEN_ROUTER_KEY=k\nLAMBDA_MEMORY_ROUTER_BASE_URL=https://a b\n')), /invalid private provider environment/);
});

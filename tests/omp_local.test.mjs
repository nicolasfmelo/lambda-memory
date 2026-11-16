import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { resolveLocalEnvironment } from '../extensions/lambda-memory-local.mjs';

test('local environment ignores canonical roots and binds plain directories', async (t) => {
  const base = mkdtempSync(resolve('.omp-local-test-'));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const ignored = join(base, 'ignored');
  const child = join(ignored, 'child');
  const plain = join(base, 'plain');
  const alias = join(base, 'alias');
  mkdirSync(child, { recursive: true });
  mkdirSync(plain);
  execFileSync('git', ['init', '-q', plain]);
  symlinkSync(child, alias);
  const config = { ignoreRoots: [realpathSync(ignored)], runtimeEnv: {}, mode: 'local' };
  assert.equal(await resolveLocalEnvironment(config, child), null);
  assert.equal(await resolveLocalEnvironment(config, alias), null);
  const env = await resolveLocalEnvironment(config, plain);
  assert.equal(env.LAMBDA_MEMORY_PROJECT, `path:${realpathSync(plain)}`);
  assert.equal(env.LAMBDA_MEMORY_CWD, resolve(plain));
});

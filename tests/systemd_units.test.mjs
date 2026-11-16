import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const section = (text, name) => text.match(new RegExp(`^\\[${name}\\]\\s*\\n([^\\[]*)`, 'm'))?.[1] ?? '';
const value = (text, key) => text.match(new RegExp(`^${key}=(.*)$`, 'm'))?.[1];

test('main unit limits restarts and alerts on failure', () => {
  const unit = section(read('runtime/lambda-memory.service'), 'Unit');
  assert.equal(value(unit, 'StartLimitIntervalSec'), '300');
  assert.equal(value(unit, 'StartLimitBurst'), '5');
  assert.equal(value(unit, 'OnFailure'), 'lambda-memory-failure.service');
});

test('main unit runs the runtime-owned node', () => {
  const exec = value(section(read('runtime/lambda-memory.service'), 'Service'), 'ExecStart');
  assert.ok(exec?.startsWith('%h/.local/share/lambda-memory/node/bin/node '));
  assert.ok(exec.endsWith('local-service.mjs'));
});

test('failure unit notifies once', () => {
  const unit = read('runtime/lambda-memory-failure.service');
  const service = section(unit, 'Service');
  assert.equal(value(service, 'Type'), 'oneshot');
  const exec = value(service, 'ExecStart');
  assert.ok(exec?.startsWith('/usr/bin/notify-send'));
  assert.ok(exec.includes('systemctl --user status lambda-memory.service'));
  assert.doesNotMatch(unit, /^\[Install\]/m);
});

test('readme bridge uses the runtime node', () => {
  const readme = read('README.md');
  assert.ok(readme.includes('node/bin/node %s/runtime/runtime/stdio.mjs'));
  assert.ok(readme.includes('sha256sum -c -'));
});

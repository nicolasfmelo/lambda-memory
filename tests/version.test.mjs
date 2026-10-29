import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { VERSION } from '../runtime/version.mjs';

test('the bridge version matches the crate version', () => {
  const cargo = readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8');
  assert.equal(VERSION, cargo.match(/^version = "([^"]+)"/m)[1]);
});

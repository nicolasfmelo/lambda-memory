import assert from 'node:assert/strict';
import { test } from 'node:test';
import { shortHash } from '../runtime/action-episode.mjs';
import { PROMPT_CHARS, REPLY_CHARS, turnText, exchangeSource, exchangeContent } from '../runtime/exchange.mjs';

test('exchange source is stable and uses turn key or prompt', () => {
  assert.equal(exchangeSource('session', 'turn', 'prompt'), exchangeSource('session', 'turn', 'other'));
  assert.notEqual(exchangeSource('session', 'turn', 'prompt'), exchangeSource('session', 'other', 'prompt'));
  assert.equal(exchangeSource('session', null, 'prompt'), `${shortHash('session')}/turn-${shortHash('prompt')}`);
});

test('turn text sanitizes before truncating by code point', () => {
  assert.equal(turnText(`ghp_${'A'.repeat(36)} 😀xyz`, {}, 12), '[REDACTED] 😀');
  assert.equal(PROMPT_CHARS, 2000);
  assert.equal(REPLY_CHARS, 4000);
  assert.equal(turnText('\n'.repeat(2500), {}, PROMPT_CHARS).length, 2000);
  assert.equal(turnText('\n'.repeat(4500), {}, REPLY_CHARS).length, 4000);
});

test('exchange content respects the byte limit and shortens reply first', () => {
  const prompt = '😀'.repeat(2000);
  const content = exchangeContent(prompt, '😀'.repeat(4000));
  assert.ok(Buffer.byteLength(content) <= 8192);
  const body = JSON.parse(content);
  assert.equal(body.prompt, prompt);
  assert.ok(Array.from(body.reply).length < 4000);
  const escaped = exchangeContent('\n'.repeat(2000), '\n'.repeat(4000));
  assert.ok(Buffer.byteLength(escaped) <= 8192);
  assert.equal(JSON.parse(escaped).prompt.length, 2000);
});

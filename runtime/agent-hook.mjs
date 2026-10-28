#!/usr/bin/env node
import { closeSync, fstatSync, openSync, writeSync, statSync, readFileSync, writeFileSync, renameSync, unlinkSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { homedir, tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { NativeMcpClient, playbookMessage } from '../extensions/lambda-memory.mjs';
import { buildEpisode, sanitize, shortHash } from './action-episode.mjs';
import * as claudeCode from './hook-adapters/claude-code.mjs';
import * as codex from './hook-adapters/codex.mjs';
import { clientEnvironment, loadConfig, resolveProjectBindingAsync } from './project-binding.mjs';

const ADAPTERS = { 'claude-code': claudeCode, codex };
const DEADLINES_MS = { 'session-start': 3000, 'post-tool-use': 2000, 'user-prompt-submit': 2000, 'stop': 2000 };
const CLEANUP_MS = 300;
const MAX_INPUT_BYTES = 4 * 1024 * 1024;

async function readInput() {
  const chunks = [];
  let size = 0;
  let over = false;
  for await (const chunk of process.stdin) {
    size += chunk.length;
    if (size > MAX_INPUT_BYTES) over = true;
    else chunks.push(chunk);
  }
  if (over) throw new Error('hook input bound');
  const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw new Error('hook input must be an object');
  return input;
}

function counterPath(prefix, sessionId) {
  return join(process.env.XDG_RUNTIME_DIR || tmpdir(), `lambda-memory-${prefix}-${shortHash(sessionId)}`);
}
function priorEpisodes(path) {
  try { return statSync(path).size; } catch { return 0; }
}
function recordEpisode(path) {
  const fd = openSync(path, 'a', 0o600);
  try { writeSync(fd, '.'); return fstatSync(fd).size; } finally { closeSync(fd); }
}
async function connect(cwd, holder, endTime, config = loadConfig()) {
  if (typeof cwd !== 'string' || !isAbsolute(cwd)) throw new Error('absolute cwd required');
  const binding = await resolveProjectBindingAsync(cwd);
  const timeoutMs = endTime - Date.now();
  if (timeoutMs <= 0) throw new Error('hook deadline');
  holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
  return { client: holder.client, binding };
}
async function sessionStart(input, adapter, holder, endTime) {
  const { client, binding } = await connect(adapter.sessionCwd(input), holder, endTime);
  const timeoutMs = endTime - Date.now();
  if (timeoutMs <= 0) return '';
  const message = playbookMessage(await client.callTool('memory_playbook', { project: binding.project }, { mutating: false, timeoutMs }));
  return message ? adapter.sessionContext(message) : '';
}
function validCall(call) {
  return call && typeof call === 'object' && !Array.isArray(call)
    && ['sessionId', 'callId', 'cwd', 'tool', 'kind', 'summary', 'intent', 'outcome', 'errorText'].every(key => typeof call[key] === 'string')
    && call.sessionId && call.callId && isAbsolute(call.cwd)
    && ['shell', 'edit', 'write', 'task', 'other'].includes(call.kind)
    && ['succeeded', 'failed', 'unknown'].includes(call.outcome);
}
async function postToolUse(input, adapter, holder, endTime) {
  const calls = adapter.calls(input);
  if (!Array.isArray(calls)) return '';
  for (const call of calls) {
    if (!validCall(call) || Date.now() >= endTime) continue;
    const config = loadConfig();
    const binding = await resolveProjectBindingAsync(call.cwd);
    if (Date.now() >= endTime) break;
    const path = counterPath(adapter.counterPrefix, call.sessionId);
    const episode = buildEpisode({ ...call, seq: priorEpisodes(path), home: homedir(), root: binding.cwd });
    if (!episode) continue;
    recordEpisode(path);
    const timeoutMs = endTime - Date.now();
    if (timeoutMs <= 0) break;
    if (!holder.client || holder.cwd !== binding.cwd) {
      if (holder.client) await holder.client.dispose({ shutdownMs: CLEANUP_MS / 2 });
      holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
      holder.cwd = binding.cwd;
    }
    await holder.client.capture({ project: binding.project, source: episode.source, kind: episode.kind, outcome: episode.outcome, origin: adapter.origin, content: episode.content }, { timeoutMs });
  }
  return '';
}
function promptPath(adapter, sessionId) {
  return join(process.env.XDG_RUNTIME_DIR || tmpdir(), `lambda-memory-${adapter.counterPrefix}-prompt-${shortHash(sessionId)}`);
}
function truncate(text, limit) {
  return Array.from(text).slice(0, limit).join('');
}
function exchangeContent(prompt, reply) {
  const body = { v: 1, prompt, reply };
  let content = JSON.stringify(body);
  for (const key of ['reply', 'prompt']) {
    if (Buffer.byteLength(content) <= 8192) break;
    const chars = Array.from(body[key]);
    let low = 0, high = chars.length;
    while (Buffer.byteLength(content) > 8192 && low < high) {
      const mid = Math.floor((low + high) / 2);
      body[key] = chars.slice(0, mid).join('');
      const candidate = JSON.stringify(body);
      if (Buffer.byteLength(candidate) <= 8192) low = mid + 1;
      else high = mid;
    }
    if (low < chars.length) {
      body[key] = chars.slice(0, Math.max(0, low - 1)).join('');
      content = JSON.stringify(body);
    }
  }
  return content;
}
async function conversation(input, adapter, mode, holder, endTime) {
  const config = loadConfig();
  if (!config.captureConversation) return '';
  const exchange = mode === 'user-prompt-submit' ? adapter.exchangePrompt(input) : adapter.exchangeStop(input);
  if (!exchange) return '';
  const binding = await resolveProjectBindingAsync(exchange.cwd);
  if (Date.now() >= endTime) return '';
  const path = promptPath(adapter, exchange.sessionId);
  const options = { home: homedir(), root: binding.cwd };
  if (mode === 'user-prompt-submit') {
    const prompt = truncate(sanitize(exchange.prompt, options), 2000);
    const temporary = `${path}-${randomUUID()}`;
    try {
      writeFileSync(temporary, prompt, { mode: 0o600, flag: 'wx' });
      renameSync(temporary, path);
    } finally {
      try { unlinkSync(temporary); } catch {}
    }
    return '';
  }
  let prompt = '';
  try { prompt = readFileSync(path, 'utf8'); } catch (error) { if (error.code !== 'ENOENT') throw error; }
  try { unlinkSync(path); } catch (error) { if (error.code !== 'ENOENT') throw error; }
  const reply = truncate(sanitize(exchange.reply, options), 4000);
  if (!prompt && !reply) return '';
  const timeoutMs = endTime - Date.now();
  if (timeoutMs <= 0) return '';
  holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
  await holder.client.capture({ project: binding.project, source: `${shortHash(exchange.sessionId)}/turn-${shortHash(exchange.turnKey ?? prompt)}`, kind: 'exchange', outcome: 'unknown', origin: `${adapter.origin.split('://')[0]}://exchange`, content: exchangeContent(prompt, reply) }, { timeoutMs });
  return '';
}
const sleep = ms => new Promise(done => { setTimeout(done, ms).unref(); });

export async function runHook(harness, mode, readStdin = readInput) {
  const adapter = Object.hasOwn(ADAPTERS, harness) ? ADAPTERS[harness] : null;
  const deadline = Object.hasOwn(DEADLINES_MS, mode) ? DEADLINES_MS[mode] : 0;
  if (!adapter || !deadline) return '';
  const holder = { client: null };
  const workMs = deadline - CLEANUP_MS;
  const endTime = Date.now() + workMs;
  let output = '';
  try {
    const work = (async () => {
      const input = await readStdin();
      if (!input || typeof input !== 'object' || Array.isArray(input)) return '';
      if (Date.now() >= endTime) return '';
      if (mode === 'session-start') return sessionStart(input, adapter, holder, endTime);
      if (mode === 'post-tool-use') return postToolUse(input, adapter, holder, endTime);
      return conversation(input, adapter, mode, holder, endTime);
    })();
    work.catch(() => {});
    output = await Promise.race([work, sleep(workMs).then(() => '')]);
  } catch { output = ''; }
  if (holder.client) {
    await Promise.race([Promise.resolve().then(() => holder.client.dispose({ shutdownMs: CLEANUP_MS / 2 })).catch(() => {}), sleep(CLEANUP_MS)]);
  }
  return output;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runHook(process.argv[2], process.argv[3]).then(
    output => (output ? new Promise(done => process.stdout.write(`${output}\n`, done)) : undefined),
    () => undefined,
  ).finally(() => process.exit(0));
}

#!/usr/bin/env node
import { closeSync, fstatSync, openSync, writeSync, statSync, readFileSync, writeFileSync, renameSync, unlinkSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { homedir, tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { NativeMcpClient } from '../extensions/lambda-memory.mjs';
import { buildEpisode, sanitize, shortHash } from './action-episode.mjs';
import { RECALL_DEADLINE_MS, decide, recallTask, advance, statePath, loadState, saveState, formatRecall, formatBrief } from './recall-gate.mjs';
import { PROMPT_CHARS, REPLY_CHARS, turnText, exchangeSource, exchangeContent } from './exchange.mjs';
import * as claudeCode from './hook-adapters/claude-code.mjs';
import * as codex from './hook-adapters/codex.mjs';
import { clientEnvironment, loadConfig, resolveCallBindingAsync } from './project-binding.mjs';

export const UNAVAILABLE_CONTEXT = 'Lambda Memory is unavailable in this session: the local service did not respond, so memory recall and capture are off. Status: systemctl --user status lambda-memory.service';

const ADAPTERS = { 'claude-code': claudeCode, codex };
const DEADLINES_MS = { 'session-end': 1200, 'session-start': 3000, 'post-tool-use': 2000, 'user-prompt-submit': 2000, 'stop': 2000, recall: 4700, 'pre-compact': 2000 };
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
async function sessionStart(input, adapter, holder, endTime) {
  const config = loadConfig();
  const binding = await resolveCallBindingAsync(adapter.sessionCwd(input), { sessionRoot: adapter.sessionRoot(input, process.env), ignoreRoots: config.ignoreRoots });
  if (!binding || Date.now() >= endTime) return '';
  holder.bindingResolved = true;
  const timeoutMs = endTime - Date.now();
  if (timeoutMs <= 0) return adapter.sessionContext(UNAVAILABLE_CONTEXT);
  holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
  await holder.client.ensureReady();
  if (!holder.client.tools.has('memory_brief')) return '';
  const remaining = endTime - Date.now();
  if (remaining <= 0) return adapter.sessionContext(UNAVAILABLE_CONTEXT);
  const result = await holder.client.callTool('memory_brief', { project: binding.project }, { mutating: false, timeoutMs: remaining });
  const message = formatBrief(resultPayload(result));
  return message ? adapter.sessionContext(message) : '';
}
async function sessionEnd(input, adapter, holder, endTime) {
  const call = adapter.sessionEnd?.(input);
  if (!call) return '';
  const config = loadConfig();
  const binding = await resolveCallBindingAsync(call.cwd, { sessionRoot: adapter.sessionRoot(input, process.env), ignoreRoots: config.ignoreRoots });
  if (!binding || Date.now() >= endTime) return '';
  const timeoutMs = endTime - Date.now();
  holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
  await holder.client.callTool('memory_session_end', { project: binding.project, session: call.sessionId }, {
    mutating: true, timeoutMs, canAdmit: () => Date.now() < endTime,
  });
  return '';
}
function resultPayload(result) {
  if (!result || result.isError) return null;
  try { return JSON.parse((Array.isArray(result.content) ? result.content : []).map(part => typeof part?.text === 'string' ? part.text : '').join('\n')); }
  catch { return null; }
}
async function recall(input, adapter, holder, endTime) {
  const call = adapter.recallPrompt(input);
  if (!call) return '';
  const config = loadConfig();
  const binding = await resolveCallBindingAsync(call.cwd, { sessionRoot: adapter.sessionRoot(input, process.env), ignoreRoots: config.ignoreRoots });
  if (!binding || Date.now() >= endTime) return '';
  const clean = sanitize(call.prompt, { home: homedir(), root: binding.cwd });
  const path = statePath(process.env.XDG_RUNTIME_DIR || tmpdir(), adapter.counterPrefix, call.sessionId);
  const state = loadState(path);
  const decision = decide(state, clean);
  let text = '';
  let nodes = [];
  if (decision.recall) {
    // Reserve time to persist the attempt before the hook's outer deadline.
    const timeoutMs = Math.min(RECALL_DEADLINE_MS, endTime - Date.now() - 50);
    if (timeoutMs > 0) {
      let timer;
      let expired = false;
      try {
        holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
        const work = holder.client.callTool('memory_recall', { task: recallTask(state, clean), projects: [binding.project], budget_bytes: 6000, exclude: state.injected }, {
          mutating: false, timeoutMs, canAdmit: () => !expired && Date.now() < endTime,
        });
        const result = await Promise.race([work, new Promise((_, reject) => { timer = setTimeout(() => { expired = true; reject(new Error('recall deadline')); }, timeoutMs); })]);
        ({ text, nodes } = formatRecall(resultPayload(result)));
      } catch { /* A failed recall is still recorded as an attempt. */ }
      finally { expired = true; clearTimeout(timer); }
    }
  }
  try { saveState(path, advance(state, clean, decision, nodes)); } catch { /* State persistence must not block the turn. */ }
  return text ? adapter.promptContext(text) : '';
}
function preCompact(input, adapter) {
  const call = adapter.preCompact?.(input);
  if (!call) return '';
  const path = statePath(process.env.XDG_RUNTIME_DIR || tmpdir(), adapter.counterPrefix, call.sessionId);
  const state = loadState(path);
  try { saveState(path, { ...state, compacted: true }); } catch { /* Compaction must continue. */ }
  const ids = state.injected.slice(-32);
  return `Lambda Memory: keep in the summary the decisions recalled from memory in this session and their node IDs${ids.length ? `: ${ids.join(', ')}` : ''}.`;
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
    const binding = await resolveCallBindingAsync(call.cwd, { sessionRoot: adapter.sessionRoot(input, process.env), ignoreRoots: config.ignoreRoots });
    if (Date.now() >= endTime) break;
    if (!binding) continue;
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
async function conversation(input, adapter, mode, holder, endTime) {
  const config = loadConfig();
  if (!config.captureConversation) return '';
  const exchange = mode === 'user-prompt-submit' ? adapter.exchangePrompt(input) : adapter.exchangeStop(input);
  if (!exchange) return '';
  const binding = await resolveCallBindingAsync(exchange.cwd, { sessionRoot: adapter.sessionRoot(input, process.env), ignoreRoots: config.ignoreRoots });
  if (!binding || Date.now() >= endTime) return '';
  const path = promptPath(adapter, exchange.sessionId);
  const options = { home: homedir(), root: binding.cwd };
  if (mode === 'user-prompt-submit') {
    const prompt = turnText(exchange.prompt, options, PROMPT_CHARS);
    const temporary = `${path}-${randomUUID()}`;
    try {
      writeFileSync(temporary, JSON.stringify({ turnKey: exchange.turnKey, prompt }), { mode: 0o600, flag: 'wx' });
      renameSync(temporary, path);
    } finally {
      try { unlinkSync(temporary); } catch {}
    }
    return '';
  }
  let prompt = '';
  try {
    const stash = JSON.parse(readFileSync(path, 'utf8'));
    if (stash && typeof stash.prompt === 'string' && (stash.turnKey === null || typeof stash.turnKey === 'string')
        && (stash.turnKey === null || exchange.turnKey === null || stash.turnKey === exchange.turnKey)) prompt = stash.prompt;
  } catch (error) { if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) throw error; }
  try { unlinkSync(path); } catch (error) { if (error.code !== 'ENOENT') throw error; }
  const reply = turnText(exchange.reply, options, REPLY_CHARS);
  if (!prompt && !reply) return '';
  const timeoutMs = endTime - Date.now();
  if (timeoutMs <= 0) return '';
  holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
  await holder.client.capture({ project: binding.project, source: exchangeSource(exchange.sessionId, exchange.turnKey, prompt), kind: 'exchange', outcome: 'unknown', origin: `${adapter.origin.split('://')[0]}://exchange`, content: exchangeContent(prompt, reply) }, { timeoutMs });
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
      if (mode === 'session-end') return sessionEnd(input, adapter, holder, endTime);
      if (mode === 'post-tool-use') return postToolUse(input, adapter, holder, endTime);
      if (mode === 'recall') return recall(input, adapter, holder, endTime);
      if (mode === 'pre-compact') return preCompact(input, adapter);
      return conversation(input, adapter, mode, holder, endTime);
    })();
    work.catch(() => {});
    output = await Promise.race([work, sleep(workMs).then(() => mode === 'session-start' && holder.bindingResolved ? adapter.sessionContext(UNAVAILABLE_CONTEXT) : '')]);
  } catch { output = mode === 'session-start' && holder.bindingResolved ? adapter.sessionContext(UNAVAILABLE_CONTEXT) : ''; }
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

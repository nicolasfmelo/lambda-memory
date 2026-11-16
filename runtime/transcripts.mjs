import { createReadStream } from 'node:fs';
import { homedir } from 'node:os';
import { basename, join } from 'node:path';
import { createInterface } from 'node:readline';

// Error output is only ever used for its first line; bound what a huge result keeps alive.
const ERROR_TEXT_CAP = 4000;

/**
 * One completed tool call: `Call = { sessionId, callId, tool, input, isError, errorText, timestampMs, seq }`.
 * `seq` is the transcript position among paired calls, in the order the results arrived.
 */

export function transcriptDir(agent, cwd, home = homedir()) {
  if (typeof cwd !== 'string' || !cwd) throw new Error('project directory required');
  if (agent === 'omp') {
    const rest = cwd === home || cwd.startsWith(`${home}/`) ? cwd.slice(home.length) : cwd;
    return join(home, '.omp/agent/sessions', (rest || '/').replaceAll('/', '-'));
  }
  if (agent === 'claude-code') {
    // Claude Code also flattens whitespace ("My App" is stored as "-…-My-App").
    return join(home, '.claude/projects', cwd.replace(/[/.\s]/g, '-'));
  }
  throw new Error('agent must be omp or claude-code');
}

async function* records(file, stats) {
  const lines = createInterface({ input: createReadStream(file, { encoding: 'utf8' }), crlfDelay: Infinity });
  for await (const line of lines) {
    if (!line.trim()) continue;
    let record;
    try { record = JSON.parse(line); } catch { record = null; }
    if (record && typeof record === 'object' && !Array.isArray(record)) yield record;
    else stats.malformed = (stats.malformed ?? 0) + 1;
  }
}

function textOf(content) {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content.map((part) => (typeof part?.text === 'string' ? part.text : '')).filter(Boolean).join('\n');
}

function stamp(record, fallback = 0) {
  const parsed = Date.parse(record?.timestamp);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function turnTextParts(content) {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content.filter(part => part?.type === 'text' && typeof part.text === 'string').map(part => part.text).join('\n');
}

const HARNESS_TAG = /^<(?:task-notification|command-name|command-message|command-args|local-command-stdout|local-command-stderr|local-command-caveat|system-reminder|bash-input|bash-stdout|bash-stderr)>/;

/** Raw conversation turns, bounded by the optional snapshot cutoff. */
export async function* readClaudeTurns(file, stats = {}, cutoffMs = Infinity) {
  let turn;
  for await (const record of records(file, stats)) {
    const timestampMs = stamp(record, NaN);
    if (!Number.isFinite(timestampMs) || timestampMs > cutoffMs) continue;
    const content = record.message?.content;
    const toolResult = Array.isArray(content) && content.some(part => part?.type === 'tool_result');
    if (record.type === 'assistant') {
      if (turn) turn.reply = turnTextParts(content);
      continue;
    }
    if (record.type !== 'user' || toolResult) continue;
    if (turn) { yield turn; turn = undefined; }
    if (record.isMeta || record.isSidechain || record.isCompactSummary || record.isVisibleInTranscriptOnly) continue;
    let prompt = turnTextParts(content);
    const name = /<command-name>([\s\S]*?)<\/command-name>/.exec(prompt);
    if (name) {
      const args = /<command-args>([\s\S]*?)<\/command-args>/.exec(prompt);
      if (!args?.[1].trim() || !name[1].trim().startsWith('/')) continue;
      prompt = `${name[1].trim()} ${args[1].trim()}`;
    } else if (HARNESS_TAG.test(prompt.trimStart())) continue;
    if (!prompt.trim() || /^\/\S+\s*$/.test(prompt)) continue;
    if (typeof record.promptId !== 'string' || !record.promptId) { stats.no_turn_key = (stats.no_turn_key ?? 0) + 1; continue; }
    if (typeof record.sessionId !== 'string' || !record.sessionId) continue;
    turn = { sessionId: record.sessionId, turnKey: record.promptId, cwd: record.cwd, prompt, reply: '', timestampMs };
  }
  if (turn) yield turn;
}

/** OMP sessions may have a title before their session header. */
export async function* readOmpTurns(file, stats = {}, cutoffMs = Infinity) {
  const entries = [];
  let session;
  for await (const record of records(file, stats)) {
    const timestampMs = stamp(record, NaN);
    if (!Number.isFinite(timestampMs) || timestampMs > cutoffMs) continue;
    if (record.type === 'session') session = record;
    else if (record.type === 'message') entries.push({ record, timestampMs });
  }
  if (!session) return;
  if (Object.hasOwn(session, 'parentSession')) { stats.subagent_session = (stats.subagent_session ?? 0) + 1; return; }
  if (typeof session.id !== 'string' || !session.id) return;
  let turn;
  for (const { record, timestampMs } of entries) {
    const message = record.message;
    if (message?.role === 'assistant') { if (turn) turn.reply = turnTextParts(message.content); continue; }
    if (message?.role !== 'user') continue;
    if (turn) { yield turn; turn = undefined; }
    const prompt = turnTextParts(message.content);
    if (message.attribution === 'agent' || !prompt.trim()) continue;
    if (typeof record.id !== 'string' || !record.id) { stats.no_turn_key = (stats.no_turn_key ?? 0) + 1; continue; }
    turn = { sessionId: session.id, turnKey: `omp-entry-${record.id}`, cwd: session.cwd, prompt, reply: '', timestampMs };
  }
  if (turn) yield turn;
}

function nameOf(file) {
  return basename(file).replace(/\.jsonl$/, '').replace(/^.*_/, '');
}

function objectOrEmpty(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function finish(stats, pending) {
  stats.unpaired = (stats.unpaired ?? 0) + pending.size;
}

/** OMP session files: `toolCall` blocks in assistant messages, `toolResult` messages keyed by `toolCallId`. */
export async function* readOmpCalls(file, stats = {}) {
  const pending = new Map();
  let sessionId = nameOf(file);
  let seq = 0;
  for await (const record of records(file, stats)) {
    if (record.type === 'session' && typeof record.id === 'string' && record.id) sessionId = record.id;
    if (record.type !== 'message') continue;
    const message = record.message;
    if (message?.role === 'assistant' && Array.isArray(message.content)) {
      for (const block of message.content) {
        if (block?.type !== 'toolCall' || typeof block.id !== 'string' || typeof block.name !== 'string' || pending.has(block.id)) continue;
        pending.set(block.id, { tool: block.name, input: objectOrEmpty(block.arguments) });
      }
    } else if (message?.role === 'toolResult' && typeof message.toolCallId === 'string') {
      const call = pending.get(message.toolCallId);
      if (!call) continue;
      pending.delete(message.toolCallId);
      const isError = message.isError === true;
      yield {
        sessionId, callId: message.toolCallId, tool: call.tool, input: call.input, isError,
        errorText: isError ? textOf(message.content).slice(0, ERROR_TEXT_CAP) : '',
        timestampMs: stamp(record, Number.isFinite(message.timestamp) ? message.timestamp : 0), seq: seq++,
      };
    }
  }
  finish(stats, pending);
}

/** Claude Code project files: `tool_use` blocks in assistant lines, `tool_result` blocks in user lines. */
export async function* readClaudeCalls(file, stats = {}) {
  const pending = new Map();
  const seen = new Set();
  let seq = 0;
  for await (const record of records(file, stats)) {
    const content = record.message?.content;
    if (!Array.isArray(content)) continue;
    const sessionId = typeof record.sessionId === 'string' && record.sessionId ? record.sessionId : nameOf(file);
    if (record.type === 'assistant') {
      for (const block of content) {
        if (block?.type !== 'tool_use' || typeof block.id !== 'string' || typeof block.name !== 'string' || seen.has(block.id)) continue;
        seen.add(block.id);
        pending.set(block.id, { sessionId, tool: block.name, input: objectOrEmpty(block.input) });
      }
    } else if (record.type === 'user') {
      for (const block of content) {
        if (block?.type !== 'tool_result' || typeof block.tool_use_id !== 'string') continue;
        const call = pending.get(block.tool_use_id);
        if (!call) continue;
        pending.delete(block.tool_use_id);
        const isError = block.is_error === true;
        yield {
          sessionId: call.sessionId, callId: block.tool_use_id, tool: call.tool, input: call.input, isError,
          errorText: isError ? textOf(block.content).slice(0, ERROR_TEXT_CAP) : '',
          timestampMs: stamp(record), seq: seq++,
        };
      }
    }
  }
  finish(stats, pending);
}

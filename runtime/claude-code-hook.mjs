#!/usr/bin/env node
// Claude Code hook: `session-start` injects the project playbook, `post-tool-use` captures an action episode.
// A hook never blocks a turn: every failure exits 0 with no output, and each mode has a hard deadline.
import { closeSync, fstatSync, openSync, writeSync, statSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { NativeMcpClient, playbookMessage } from '../extensions/lambda-memory.mjs';
import { buildEpisode, shortHash } from './action-episode.mjs';
import { clientEnvironment, loadConfig, resolveProjectBindingAsync } from './project-binding.mjs';

const DEADLINES_MS = { 'session-start': 3000, 'post-tool-use': 2000 };
const CLEANUP_MS = 300; // the child process is killed inside this slice, so the whole hook meets its deadline
const MAX_INPUT_BYTES = 4 * 1024 * 1024;
const PLAYBOOK_TOOL = 'memory_playbook';
const CAPTURE_ORIGIN = 'claude-code://action';

async function readInput() {
  const chunks = [];
  let size = 0;
  let over = false;
  for await (const chunk of process.stdin) {
    size += chunk.length;
    if (size > MAX_INPUT_BYTES) over = true; // keep draining so the writer never sees a closed pipe
    else chunks.push(chunk);
  }
  if (over) throw new Error('hook input bound');
  const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw new Error('hook input must be an object');
  return input;
}

function responseText(response) {
  if (typeof response === 'string') return response;
  if (!response || typeof response !== 'object') return '';
  for (const key of ['error', 'stderr', 'content', 'message', 'stdout']) {
    const value = response[key];
    if (typeof value === 'string' && value.trim()) return value;
    if (Array.isArray(value)) {
      const text = value.map((part) => (typeof part?.text === 'string' ? part.text : '')).filter(Boolean).join('\n');
      if (text.trim()) return text;
    }
  }
  return '';
}

function responseFailed(response) {
  return response?.is_error === true || response?.success === false;
}

// One byte per kept episode: the file size is the number of prior episodes in this session.
function counterPath(sessionId) {
  return join(process.env.XDG_RUNTIME_DIR || tmpdir(), `lambda-memory-cc-${shortHash(sessionId)}`);
}

function priorEpisodes(path) {
  try { return statSync(path).size; } catch { return 0; }
}

function recordEpisode(path) {
  const fd = openSync(path, 'a', 0o600);
  try { writeSync(fd, '.'); return fstatSync(fd).size; } finally { closeSync(fd); }
}

function absoluteDirectory(input) {
  if (typeof input.cwd !== 'string' || !input.cwd.startsWith('/')) throw new Error('absolute cwd required');
  return input.cwd;
}

async function connect(input, holder, timeoutMs) {
  const config = loadConfig();
  const binding = await resolveProjectBindingAsync(absoluteDirectory(input));
  holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
  return { client: holder.client, binding };
}

async function sessionStart(input, holder, timeoutMs) {
  const { client, binding } = await connect(input, holder, timeoutMs);
  const message = playbookMessage(await client.callTool(PLAYBOOK_TOOL, { project: binding.project }, { mutating: false, timeoutMs }));
  if (!message) return '';
  return JSON.stringify({ hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: message } });
}

async function postToolUse(input, holder, timeoutMs) {
  if (typeof input.session_id !== 'string' || !input.session_id || typeof input.tool_use_id !== 'string' || !input.tool_use_id) return '';
  const config = loadConfig();
  const binding = await resolveProjectBindingAsync(absoluteDirectory(input));
  const failureEvent = input.hook_event_name === 'PostToolUseFailure';
  if (failureEvent && input.is_interrupt === true) return '';
  const path = counterPath(input.session_id);
  const failed = failureEvent || responseFailed(input.tool_response);
  const errorText = !failed ? '' : failureEvent ? (typeof input.error === 'string' ? input.error : '') : responseText(input.tool_response);
  const episode = buildEpisode({
    agent: 'claude-code', tool: input.tool_name, input: input.tool_input, isError: failed,
    errorText, sessionId: input.session_id, callId: input.tool_use_id,
    seq: priorEpisodes(path), home: homedir(), root: binding.cwd,
  });
  if (!episode) return '';
  recordEpisode(path);
  holder.client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd, timeoutMs, shutdownMs: CLEANUP_MS / 2 });
  await holder.client.capture({
    project: binding.project, source: episode.source, kind: episode.kind, outcome: episode.outcome,
    origin: CAPTURE_ORIGIN, content: episode.content,
  }, { timeoutMs });
  return '';
}

const sleep = (ms) => new Promise((done) => { setTimeout(done, ms).unref(); });

/** Returns the text to print (possibly empty); never throws. */
export async function runHook(mode, readStdin = readInput) {
  const deadline = DEADLINES_MS[mode];
  if (!deadline) return '';
  const holder = { client: null };
  const workMs = deadline - CLEANUP_MS;
  let output = '';
  try {
    const work = (async () => {
      const input = await readStdin();
      return mode === 'session-start' ? sessionStart(input, holder, workMs) : postToolUse(input, holder, workMs);
    })();
    work.catch(() => {}); // a late rejection after the deadline must not surface
    output = await Promise.race([work, sleep(workMs).then(() => '')]);
  } catch {
    output = '';
  }
  if (holder.client) {
    await Promise.race([Promise.resolve().then(() => holder.client.dispose({ shutdownMs: CLEANUP_MS / 2 })).catch(() => {}), sleep(CLEANUP_MS)]);
  }
  return output;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runHook(process.argv[2]).then(
    (output) => (output ? new Promise((done) => process.stdout.write(`${output}\n`, done)) : undefined),
    () => undefined,
  ).finally(() => process.exit(0));
}

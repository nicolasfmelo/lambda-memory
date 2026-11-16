#!/usr/bin/env node
// Snapshot and consented, idempotent import of conversation turns.
import { readdir, realpath } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { isDeepStrictEqual } from 'node:util';
import { NativeMcpClient } from '../extensions/lambda-memory.mjs';
import { containsSecretLike, shortHash } from './action-episode.mjs';
import { exchangeContent, exchangeSource, PROMPT_CHARS, REPLY_CHARS, turnText } from './exchange.mjs';
import { clientEnvironment, loadConfig, resolveCallBindingAsync } from './project-binding.mjs';
import { readClaudeTurns, readOmpTurns, transcriptDir } from './transcripts.mjs';

const READERS = { omp: readOmpTurns, 'claude-code': readClaudeTurns };
const USAGE = 'usage: node runtime/import-exchanges.mjs --agent omp|claude-code --cwd <dir> [--since <ISO>] [--include <session hash>] [--apply]';
const CONSENT = 'exact import plan consent required';
const MAX_INPUT = 1024 * 1024;
const AGE = 399 * 24 * 60 * 60 * 1000;

function argumentsOf(argv) {
  const options = { include: [], apply: false };
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i];
    if (flag === '--apply') options.apply = true;
    else if (['--agent', '--cwd', '--since', '--include'].includes(flag) && i + 1 < argv.length && !argv[i + 1].startsWith('--')) {
      const value = argv[++i];
      if (flag === '--include') options.include.push(value); else options[flag.slice(2)] = value;
    } else throw new Error(USAGE);
  }
  if (!Object.hasOwn(READERS, options.agent) || !options.cwd) throw new Error(USAGE);
  if (options.since !== undefined) {
    options.sinceMs = Date.parse(options.since);
    if (!Number.isFinite(options.sinceMs)) throw new Error(USAGE);
  }
  return options;
}

async function readPlan() {
  const chunks = []; let size = 0;
  for await (const chunk of process.stdin) {
    size += chunk.length;
    if (size > MAX_INPUT) throw new Error(CONSENT);
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString('utf8')); } catch { throw new Error(CONSENT); }
}

async function filesIn(dir) {
  try { return (await readdir(dir)).filter(name => name.endsWith('.jsonl')).sort().map(name => join(dir, name)); }
  catch (error) { if (error.code === 'ENOENT') return []; throw error; }
}

async function buildPlan(options, config, target, cwd, cutoff, includes) {
  const home = homedir();
  const skipped = {};
  const skip = (key, n = 1) => { if (n) skipped[key] = (skipped[key] ?? 0) + n; };
  const all = [];
  for (const file of await filesIn(transcriptDir(options.agent, cwd, home))) {
    const stats = {};
    for await (const turn of READERS[options.agent](file, stats, cutoff)) all.push(turn);
    skip('malformed_line', stats.malformed ?? 0); skip('no_turn_key', stats.no_turn_key ?? 0); skip('subagent_session', stats.subagent_session ?? 0);
  }
  const sessions = new Map();
  for (const turn of all) {
    let session = sessions.get(turn.sessionId);
    if (!session) { session = { first: turn, flagged: false }; sessions.set(turn.sessionId, session); }
    if (turn.timestampMs < session.first.timestampMs) session.first = turn;
    if (containsSecretLike(turn.prompt) || containsSecretLike(turn.reply)) session.flagged = true;
  }
  const flagged = new Map([...sessions].filter(([, s]) => s.flagged).map(([id, s]) => [shortHash(id), s]));
  if (includes.some(hash => typeof hash !== 'string' || !flagged.has(hash))) throw new Error('unknown included session hash');
  const included = [...new Set(includes)].sort();
  const includedSet = new Set(included);
  const excluded = [...flagged].filter(([hash]) => !includedSet.has(hash)).map(([session, s]) => ({
    session, first_at: new Date(s.first.timestampMs).toISOString(),
    first_prompt: turnText(s.first.prompt, { home, root: target.cwd }, 120),
  })).sort((a, b) => a.session.localeCompare(b.session));
  const unique = new Map();
  for (const turn of all) {
    const key = options.agent === 'omp' ? JSON.stringify([turn.sessionId, turn.turnKey]) : turn.turnKey;
    const previous = unique.get(key);
    if (!previous || turn.timestampMs < previous.timestampMs) unique.set(key, turn);
  }
  const bindings = new Map();
  const kept = [];
  for (const turn of unique.values()) {
    const session = sessions.get(turn.sessionId);
    if (session.flagged && !includedSet.has(shortHash(turn.sessionId))) continue;
    if (turn.timestampMs < cutoff - AGE) { skip('too_old'); continue; }
    if (options.sinceMs !== undefined && turn.timestampMs < options.sinceMs) { skip('before_since'); continue; }
    const key = JSON.stringify([turn.cwd, session.first.cwd]);
    if (!bindings.has(key)) {
      try {
        const binding = await resolveCallBindingAsync(turn.cwd, { sessionRoot: session.first.cwd, ignoreRoots: config.ignoreRoots });
        bindings.set(key, binding ? { binding } : { reason: 'ignored' });
      } catch (error) {
        if (error.code === 'ENOENT' || error.code === 'ENOTDIR' || error.message === 'project directory required' || typeof turn.cwd !== 'string') bindings.set(key, { reason: 'missing_cwd' });
        else throw error;
      }
    }
    const result = bindings.get(key);
    if (result.reason) { skip(result.reason); continue; }
    if (result.binding.project !== target.project) { skip('other_project'); continue; }
    const prompt = turnText(turn.prompt, { home, root: target.cwd }, PROMPT_CHARS);
    const reply = turnText(turn.reply, { home, root: target.cwd }, REPLY_CHARS);
    kept.push({ session: shortHash(turn.sessionId), source: exchangeSource(turn.sessionId, turn.turnKey, prompt), prompt, reply, timestampMs: turn.timestampMs });
  }
  return { kept, plan: {
    agent: options.agent, project: target.project, cutoff_ms: cutoff,
    sessions: [...new Set(kept.map(turn => turn.session))].sort(), turns: kept.length,
    excluded_flagged: excluded, included_flagged: included, skipped,
    estimated_curation_calls: Math.ceil(kept.length / 4),
    samples: kept.slice(0, 10).map(({ source, prompt, reply }) => ({ source, prompt, reply })), accept_charges: false,
  } };
}

function statusOf(result, source) {
  let body = result?.structuredContent;
  if (!body) {
    try { body = JSON.parse((result?.content ?? []).map(part => part.text ?? '').join('\n')); } catch { return 'rejected'; }
  }
  if (result?.isError || body?.error) {
    if (body?.error === 'pending capture capacity reached') return 'pending_limit';
    if (body?.error === 'lifecycle or source conflict') return 'already_live';
    return 'rejected';
  }
  if (body?.source !== source) return 'rejected';
  if (body.status === 'saved_pending') return 'saved';
  if (body.status === 'already_saved_pending') return 'already_saved';
  return 'rejected';
}

export async function main(argv) {
  const options = argumentsOf(argv);
  const input = options.apply ? await readPlan() : null;
  if (options.apply && (!input || input.accept_charges !== true || !Number.isSafeInteger(input.cutoff_ms) || input.cutoff_ms > Date.now() || !Array.isArray(input.included_flagged))) throw new Error(CONSENT);
  const config = loadConfig();
  const cwd = await realpath(resolve(options.cwd));
  const target = await resolveCallBindingAsync(cwd, { sessionRoot: cwd, ignoreRoots: config.ignoreRoots });
  if (!target) throw new Error('the directory is under an ignored root');
  let snapshot;
  try { snapshot = await buildPlan(options, config, target, cwd, options.apply ? input.cutoff_ms : Date.now(), options.apply ? input.included_flagged : options.include); }
  catch (error) { if (options.apply && error.message === 'unknown included session hash') throw new Error(CONSENT); throw error; }
  if (!options.apply) return { output: snapshot.plan, failed: false };
  if (!isDeepStrictEqual({ ...input, accept_charges: false }, snapshot.plan)) throw new Error(CONSENT);
  const totals = { outcome: 'completed', saved: 0, already_saved: 0, already_live: 0, rejected: 0 };
  let consecutive = 0;
  const client = new NativeMcpClient({ env: clientEnvironment(config, target), cwd: target.cwd });
  try {
    for (const turn of snapshot.kept) {
      let status;
      try {
        status = statusOf(await client.capture({ project: target.project, source: turn.source, kind: 'exchange', outcome: 'unknown', origin: `import://${options.agent}`, content: exchangeContent(turn.prompt, turn.reply), recorded_at_ms: turn.timestampMs }), turn.source);
      } catch { status = 'rejected'; }
      if (status === 'pending_limit') { totals.outcome = 'pending_limit'; break; }
      totals[status]++;
      consecutive = status === 'rejected' ? consecutive + 1 : 0;
      if (consecutive >= 10) { totals.outcome = 'failed'; break; }
    }
  } finally { await client.dispose({ shutdownMs: 1000 }).catch(() => {}); }
  return { output: totals, failed: totals.outcome === 'failed' };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).then(({ output, failed }) => {
    if (failed) process.exitCode = 2;
    process.stdout.write(`${JSON.stringify(output)}\n`, () => {});
  }, error => { process.stderr.write(`${error.message}\n`); process.exitCode = 2; });
}

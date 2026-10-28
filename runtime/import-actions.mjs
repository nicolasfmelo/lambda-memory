#!/usr/bin/env node
// One-time history import: rebuilds action episodes from OMP / Claude Code transcripts.
// Dry run by default; --apply captures through the bridge (capture is idempotent per source).
import { readdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { NativeMcpClient } from '../extensions/lambda-memory.mjs';
import { buildEpisode } from './action-episode.mjs';
import { describeOmp, describeClaudeCode } from './hook-adapters/tool-kinds.mjs';
import { clientEnvironment, loadConfig, resolveProjectBindingAsync } from './project-binding.mjs';
import { readClaudeCalls, readOmpCalls, transcriptDir } from './transcripts.mjs';

const USAGE = 'usage: node runtime/import-actions.mjs --agent omp|claude-code --cwd <dir> [--since <ISO date>] [--apply]';
const SAMPLES = 10;
const MAX_CONSECUTIVE_FAILURES = 10;
const READERS = { omp: readOmpCalls, 'claude-code': readClaudeCalls };

export function parseArguments(argv) {
  const options = { apply: false };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--apply') options.apply = true;
    else if (['--agent', '--cwd', '--since'].includes(flag) && i + 1 < argv.length && !argv[i + 1].startsWith('--')) {
      options[flag.slice(2)] = argv[++i];
    } else throw new Error(`invalid argument: ${flag}\n${USAGE}`);
  }
  if (!Object.hasOwn(READERS, options.agent)) throw new Error(`invalid --agent\n${USAGE}`);
  if (typeof options.cwd !== 'string' || !options.cwd) throw new Error(`--cwd is required\n${USAGE}`);
  if (options.since !== undefined) {
    options.sinceMs = Date.parse(options.since);
    if (!Number.isFinite(options.sinceMs)) throw new Error(`invalid --since\n${USAGE}`);
  }
  return options;
}

async function transcriptFiles(dir) {
  try {
    return (await readdir(dir)).filter((name) => name.endsWith('.jsonl')).sort().map((name) => join(dir, name));
  } catch (failure) {
    if (failure.code === 'ENOENT') return [];
    throw failure;
  }
}

/**
 * Walks every transcript and yields built episodes in transcript order.
 * Counts: calls = episodes + sum(skipped except malformed_line), which counts lines, not calls.
 * `seq` numbers the kept episodes of each session, the same way the live hook does.
 */
export async function* importEpisodes({ agent, files, root, home, sinceMs, report }) {
  const counters = new Map();
  const skip = (reason) => { report.skipped[reason] = (report.skipped[reason] ?? 0) + 1; };
  for (const file of files) {
    const stats = {};
    for await (const call of READERS[agent](file, stats)) {
      report.calls += 1;
      if (sinceMs !== undefined && call.timestampMs < sinceMs) { skip('before_since'); continue; }
      const seq = counters.get(call.sessionId) ?? 0;
      const episode = buildEpisode({
        tool: call.tool, ...(agent === 'omp' ? describeOmp : describeClaudeCode)(call.tool, call.input),
        outcome: call.isError === true ? 'failed' : 'succeeded', errorText: call.errorText,
        sessionId: call.sessionId, callId: call.callId, seq, home, root,
      });
      if (!episode) { skip('no_episode'); continue; }
      counters.set(call.sessionId, seq + 1);
      report.episodes += 1;
      yield { ...episode, origin: `import://${agent}`, recorded_at_ms: call.timestampMs };
    }
    report.calls += stats.unpaired ?? 0;
    if (stats.unpaired) report.skipped.unpaired_call = (report.skipped.unpaired_call ?? 0) + stats.unpaired;
    if (stats.malformed) report.skipped.malformed_line = (report.skipped.malformed_line ?? 0) + stats.malformed;
  }
}

function receiptStatus(result, source) {
  if (!result || result.isError) return 'rejected';
  let receipt = result.structuredContent;
  if (!receipt) {
    const text = (Array.isArray(result.content) ? result.content : []).map((part) => (typeof part?.text === 'string' ? part.text : '')).join('\n');
    try { receipt = JSON.parse(text); } catch { return 'rejected'; }
  }
  if (receipt?.source !== source) return 'rejected';
  if (receipt.status === 'saved_pending') return 'saved';
  if (receipt.status === 'already_saved_pending') return 'already_saved';
  return 'rejected';
}

export async function main(argv) {
  const options = parseArguments(argv);
  const config = options.apply ? loadConfig() : null;
  const binding = await resolveProjectBindingAsync(resolve(options.cwd));
  const home = homedir();
  const files = await transcriptFiles(transcriptDir(options.agent, resolve(options.cwd), home));
  const report = { calls: 0, episodes: 0, skipped: {} };
  const episodes = importEpisodes({ agent: options.agent, files, root: binding.cwd, home, sinceMs: options.sinceMs, report });

  if (!options.apply) {
    const samples = [];
    for await (const episode of episodes) if (samples.length < SAMPLES) samples.push(episode);
    return { output: { ...report, samples }, failed: false };
  }

  const client = new NativeMcpClient({ env: clientEnvironment(config, binding), cwd: binding.cwd });
  const totals = { saved: 0, already_saved: 0, rejected: 0 };
  let consecutiveFailures = 0;
  let failed = false;
  try {
    for await (const episode of episodes) {
      let status;
      try {
        status = receiptStatus(await client.capture({
          project: binding.project, source: episode.source, kind: episode.kind, outcome: episode.outcome,
          origin: episode.origin, content: episode.content, recorded_at_ms: episode.recorded_at_ms,
        }), episode.source);
        consecutiveFailures = 0;
      } catch {
        // Indeterminate: the capture may have landed; a rerun reports it as already_saved. Never retried here.
        status = 'rejected';
        consecutiveFailures += 1;
      }
      totals[status] += 1;
      if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES) { failed = true; break; }
    }
  } finally {
    await client.dispose({ shutdownMs: 1000 }).catch(() => {});
  }
  return { output: totals, failed };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).then(({ output, failed }) => {
    process.stdout.write(`${JSON.stringify(output)}\n`);
    if (failed) {
      process.stderr.write('the memory service stopped accepting captures; import aborted (rerun is safe)\n');
      process.exitCode = 2;
    }
  }, (failure) => {
    process.stderr.write(`${failure.message}\n`);
    process.exitCode = 2;
  }).finally(() => process.exit());
}

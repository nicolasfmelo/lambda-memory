#!/usr/bin/env node
import { pathToFileURL } from 'node:url';
import { runHook as runAgentHook } from './agent-hook.mjs';

export function runHook(mode, readStdin) {
  return runAgentHook('claude-code', mode, readStdin);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runHook(process.argv[2]).then(
    output => (output ? new Promise(done => process.stdout.write(`${output}\n`, done)) : undefined),
    () => undefined,
  ).finally(() => process.exit(0));
}

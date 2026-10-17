// Operator maintenance only: stop the shared service first; no automatic retry or marker reset.
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { loadConfig, resolveProjectBinding, safeEnvironment, nativeEnvironment } from './project-binding.mjs';

// Native maintenance reports only fixed RuntimeError text; nothing else is ever echoed.
const RUNTIME_REASONS = ['scope not authorized', 'invalid runtime input: ', 'lifecycle or source conflict',
  'record not found', 'pending capture capacity reached', 'runtime storage unavailable',
  'OpenRouter memory operation failed', 'memory operation has an unsettled paid admission'];

function runtimeReason(text) {
  const line = text.split('\n').map(item => item.trim()).filter(Boolean).at(-1) ?? '';
  return line.length <= 256 && RUNTIME_REASONS.some(reason => line.startsWith(reason)) ? line : '';
}

async function main() {
  const action = process.argv[2];
  if (process.argv.length !== 3 || !['inspect', 'recover'].includes(action)) {
    throw new Error('usage: node runtime/recover-pending.mjs inspect|recover');
  }
  const config = loadConfig();
  const binding = resolveProjectBinding(process.env.LAMBDA_MEMORY_CWD || process.cwd());
  const runtimeDir = process.env.XDG_RUNTIME_DIR || `/run/user/${process.getuid()}`;
  const serviceEnv = { ...safeEnvironment(), XDG_RUNTIME_DIR: runtimeDir,
    DBUS_SESSION_BUS_ADDRESS: process.env.DBUS_SESSION_BUS_ADDRESS || `unix:path=${runtimeDir}/bus` };
  const service = spawnSync('systemctl', ['--user', 'show', 'lambda-memory.service', '--property=ActiveState', '--value'], {
    env: serviceEnv, encoding: 'utf8', timeout: 5000,
  });
  // A crashed unit stays "failed" after stop; both states mean nothing owns the store.
  if (service.error || service.status !== 0 || !['inactive', 'failed'].includes(service.stdout.trim())) {
    throw new Error('authorized maintenance requires lambda-memory.service to be stopped');
  }
  let input;
  if (action === 'recover') {
    const chunks = []; let size = 0;
    for await (const chunk of process.stdin) {
      size += chunk.length;
      if (size > 32768) throw new Error('recovery input bound');
      chunks.push(chunk);
    }
    const request = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    if (request.project !== binding.project || request.accept_duplicate_charge !== true) {
      throw new Error('exact project and explicit duplicate-charge consent required');
    }
    input = JSON.stringify(request);
  }
  const store = join(config.dataDir, createHash('sha256').update(binding.project).digest('hex'), 'store');
  const child = spawn(config.nativeBin, [action === 'inspect' ? '--inspect-dream-recovery' : '--recover-dream-sources'], {
    cwd: binding.cwd,
    env: { ...safeEnvironment(), ...nativeEnvironment(config),
      LAMBDA_MEMORY_DATA_DIR: store, LAMBDA_MEMORY_OWNER: config.owner,
      LAMBDA_MEMORY_ACTOR: config.actor, LAMBDA_MEMORY_PROJECT: binding.project,
      LAMBDA_MEMORY_TOKEN: config.token },
    stdio: ['pipe', 'inherit', 'pipe'],
  });
  // Never echo provider bodies or secret-bearing launch configuration.
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', chunk => { if (stderr.length < 4096) stderr += chunk; });
  child.stdin.on('error', () => {});
  child.stdin.end(input);
  const exit = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => resolve({ code, signal }));
  });
  if (exit.code !== 0) {
    const reason = runtimeReason(stderr);
    throw new Error(`native maintenance failed or was interrupted${reason ? ` (${reason})` : ''}; do not automatically retry`);
  }
}

main().catch(error => { console.error(error.message); process.exitCode = 2; });

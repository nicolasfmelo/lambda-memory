import { realpath } from 'node:fs/promises';
import { resolve } from 'node:path';
import { install } from './lambda-memory.mjs';
import { loadConfig, resolveProjectBindingAsync, clientEnvironment, isIgnored } from '../runtime/project-binding.mjs';

// Asynchronous on purpose: synchronous git calls here would block OMP's event loop past the recall deadline.
export async function resolveLocalEnvironment(config, cwd) {
  if (isIgnored(await realpath(cwd), config.ignoreRoots)) return null;
  return { ...clientEnvironment(config, await resolveProjectBindingAsync(cwd)), LAMBDA_MEMORY_CWD: resolve(cwd) };
}

export default function installLocal(api) {
  const config = loadConfig();
  return install(api, {
    timeoutMs: config.mode === 'openrouter' ? 660000 : 10000,
    captureConversation: config.captureConversation,
    resolveEnvironment(ctx) {
      return resolveLocalEnvironment(config, ctx.cwd);
    },
  });
}

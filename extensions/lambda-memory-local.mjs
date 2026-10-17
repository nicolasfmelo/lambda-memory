import { resolve } from 'node:path';
import { install } from './lambda-memory.mjs';
import { loadConfig, resolveProjectBinding, clientEnvironment } from '../runtime/project-binding.mjs';

export default function installLocal(api) {
  const config = loadConfig();
  return install(api, {
    timeoutMs: config.mode === 'openrouter' ? 660000 : 10000,
    resolveEnvironment(ctx) {
      const binding = resolveProjectBinding(ctx.cwd);
      return { ...clientEnvironment(config, binding), LAMBDA_MEMORY_CWD: resolve(ctx.cwd) };
    },
  });
}

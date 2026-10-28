import { execFile, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { accessSync, constants, lstatSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { realpath, stat } from 'node:fs/promises';
import { homedir } from 'node:os';
import { isAbsolute, join } from 'node:path';

export function loadConfig(env = process.env) {
  const path = env.LAMBDA_MEMORY_CONFIG || join(homedir(), '.config/lambda-memory/runtime.json');
  const file = lstatSync(path);
  if (!file.isFile() || file.size > 16384 || (file.mode & 0o077) || file.uid !== process.getuid()) throw new Error('private configuration required');
  const config = JSON.parse(readFileSync(path, 'utf8'));
  for (const key of ['nativeBin', 'bridgeBin', 'dataDir', 'socketPath']) {
    if (typeof config[key] !== 'string' || !isAbsolute(config[key]) || /[\u0000-\u001f]/.test(config[key])) throw new Error('invalid configuration');
  }
  if (Buffer.byteLength(config.socketPath) > 100) throw new Error('socket path bound');
  for (const key of ['owner', 'actor']) {
    if (typeof config[key] !== 'string' || !config[key].trim() || Buffer.byteLength(config[key]) > 512 || /[\u0000-\u001f]/.test(config[key])) throw new Error('invalid identity');
  }
  if (typeof config.token !== 'string' || !/^[\x21-\x7e]{32,1024}$/.test(config.token)) throw new Error('invalid token');
  const runtimeEnv = config.runtimeEnv ?? {};
  const options = new Set(['MAX_CONTENT_BYTES', 'MAX_PENDING', 'MAX_CANDIDATES', 'CONTEXT_BYTES', 'EMBEDDING_DIMENSIONS', 'DREAM_BATCH', 'IDLE_MS', 'TICK_MS', 'RETENTION_MS'].map(name => `LAMBDA_MEMORY_${name}`));
  if (!runtimeEnv || typeof runtimeEnv !== 'object' || Array.isArray(runtimeEnv)
      || Object.entries(runtimeEnv).some(([name, value]) => !options.has(name) || typeof value !== 'string')) throw new Error('invalid runtime environment');
  const mode = config.mode ?? 'local';
  if (!['local', 'openrouter'].includes(mode)) throw new Error('invalid runtime mode');
  if (mode === 'openrouter' && (typeof config.providerEnvFile !== 'string' || !isAbsolute(config.providerEnvFile) || /[\u0000-\u001f]/.test(config.providerEnvFile))) throw new Error('private provider environment required');
  let webPort, webLeaseMs = 120000;
  if (config.webPort !== undefined) {
    if (!Number.isInteger(config.webPort) || config.webPort < 1024 || config.webPort > 65535) throw new Error('invalid web configuration');
    webPort = config.webPort;
  }
  if (config.webLeaseMs !== undefined) {
    if (!Number.isInteger(config.webLeaseMs) || config.webLeaseMs < 5000 || config.webLeaseMs > 3600000) throw new Error('invalid web configuration');
    webLeaseMs = config.webLeaseMs;
  }
  accessSync(config.nativeBin, constants.X_OK);
  accessSync(config.bridgeBin, constants.X_OK);
  return { ...config, mode, runtimeEnv, webPort, webLeaseMs, configPath: path };
}

export function nativeEnvironment(config) {
  const environment = { ...config.runtimeEnv, LAMBDA_MEMORY_MODE: config.mode };
  if (config.mode !== 'openrouter') return environment;
  const file = lstatSync(config.providerEnvFile);
  if (!file.isFile() || file.size > 16384 || (file.mode & 0o077) || file.uid !== process.getuid()) throw new Error('private provider environment required');
  const models = {
    LAMBDA_PLANNER_MODEL: 'anthropic/claude-haiku-5.5',
    LAMBDA_EMBEDDING_MODEL: 'google/gemini-embedding-2',
    LAMBDA_SELECTOR_MODEL: 'openai/gpt-6-luna-decisions',
  };
  const values = {};
  for (const line of readFileSync(config.providerEnvFile, 'utf8').split(/\r?\n/)) {
    if (!line.trim() || line.trimStart().startsWith('#')) continue;
    const match = /^(OPEN_ROUTER_KEY|LAMBDA_MEMORY_ROUTER_BASE_URL|LAMBDA_PLANNER_MODEL|LAMBDA_EMBEDDING_MODEL|LAMBDA_SELECTOR_MODEL)=([\x21-\x7e]+)$/.exec(line);
    if (!match || Object.hasOwn(values, match[1])) throw new Error('invalid private provider environment');
    values[match[1]] = match[2];
  }
  if (!values.OPEN_ROUTER_KEY || values.OPEN_ROUTER_KEY === 'REPLACE_LOCALLY_NEVER_COMMIT') throw new Error('provider key required');
  for (const [name, model] of Object.entries(models)) {
    if (values[name] !== undefined && values[name] !== model) throw new Error('unreviewed runtime model profile');
    values[name] = model;
  }
  return { ...environment, ...values };
}

const GIT_OPTIONS = { encoding: 'utf8', timeout: 1500, maxBuffer: 8192 };

function git(cwd, args) {
  try { return execFileSync('git', ['-C', cwd, ...args], { ...GIT_OPTIONS, env: safeEnvironment(), stdio: ['ignore', 'pipe', 'ignore'] }).trim(); }
  catch { return ''; }
}

function gitAsync(cwd, args) {
  return new Promise(resolveGit => {
    const child = execFile('git', ['-C', cwd, ...args], { ...GIT_OPTIONS, env: safeEnvironment() },
      (failure, stdout) => resolveGit(failure ? '' : String(stdout).trim()));
    child.stdin?.end();
  });
}

function repositoryIdentity(remote) {
  if (!remote || /[\u0000-\u0020]/.test(remote)) return '';
  let host, path;
  try {
    if (/^[a-z][a-z0-9+.-]*:\/\//i.test(remote)) {
      const url = new URL(remote);
      if (!['https:', 'http:', 'ssh:', 'git:'].includes(url.protocol)) return '';
      host = url.host; path = url.pathname;
    } else {
      const match = /^(?:[^@/:]+@)?([^/:]+):(.+)$/.exec(remote);
      if (!match) return '';
      [, host, path] = match;
    }
    path = path.replace(/^\/+|\/+$/g, '').replace(/\.git$/, '');
    if (!host || !path || path.split('/').some(part => !part || part === '.' || part === '..')) return '';
    const identity = `repo:${host.toLowerCase()}/${path}`;
    return Buffer.byteLength(identity) <= 512 ? identity : '';
  } catch { return ''; }
}

function projectIdentity(canonical, remote) {
  const fallback = `path:${canonical}`;
  const project = repositoryIdentity(remote) || (Buffer.byteLength(fallback) <= 512 ? fallback : `path:${createHash('sha256').update(canonical).digest('hex')}`);
  return { cwd: canonical, project };
}

export function resolveProjectBinding(cwd) {
  if (typeof cwd !== 'string' || !isAbsolute(cwd)) throw new Error('absolute project directory required');
  const directory = realpathSync(cwd);
  if (!statSync(directory).isDirectory()) throw new Error('project directory required');
  const root = git(directory, ['rev-parse', '--show-toplevel']);
  const canonical = root ? realpathSync(root) : directory;
  const remote = root ? git(canonical, ['remote', 'get-url', 'upstream']) || git(canonical, ['remote', 'get-url', 'origin']) : '';
  return projectIdentity(canonical, remote);
}

// Same binding without blocking the event loop; the shared daemon serves every session on it.
export async function resolveProjectBindingAsync(cwd) {
  if (typeof cwd !== 'string' || !isAbsolute(cwd)) throw new Error('absolute project directory required');
  const directory = await realpath(cwd);
  if (!(await stat(directory)).isDirectory()) throw new Error('project directory required');
  const root = await gitAsync(directory, ['rev-parse', '--show-toplevel']);
  const canonical = root ? await realpath(root) : directory;
  const remote = root ? await gitAsync(canonical, ['remote', 'get-url', 'upstream']) || await gitAsync(canonical, ['remote', 'get-url', 'origin']) : '';
  return projectIdentity(canonical, remote);
}

export function safeEnvironment(env = process.env) {
  return Object.fromEntries(['PATH', 'HOME', 'LANG', 'LC_ALL'].filter(key => typeof env[key] === 'string').map(key => [key, env[key]]));
}

export function clientEnvironment(config, binding, env = process.env) {
  return {
    ...safeEnvironment(env),
    ...config.runtimeEnv,
    LAMBDA_MEMORY_MODE: config.mode,
    LAMBDA_MEMORY_CONFIG: config.configPath,
    LAMBDA_MEMORY_BIN: config.bridgeBin,
    LAMBDA_MEMORY_OWNER: config.owner,
    LAMBDA_MEMORY_ACTOR: config.actor,
    LAMBDA_MEMORY_PROJECT: binding.project,
    LAMBDA_MEMORY_CWD: binding.cwd,
    LAMBDA_MEMORY_TOKEN: config.token,
  };
}

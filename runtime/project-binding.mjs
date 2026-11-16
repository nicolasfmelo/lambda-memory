import { execFile, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { accessSync, constants, existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { lstat, realpath, stat } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';

export function ensureStoreParent(storePath) {
  const dir = dirname(storePath);
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true, mode: 0o700 });
}

export function userIdentity(owner) {
  const identity = `user:${owner}`;
  return Buffer.byteLength(identity) <= 512 ? identity : `user:${createHash('sha256').update(owner).digest('hex')}`;
}

export function defaultIgnoreRoots(env = process.env, home = homedir()) {
  const roots = [env.XDG_CACHE_HOME || join(home, '.cache'), tmpdir(), ...(env.XDG_RUNTIME_DIR ? [env.XDG_RUNTIME_DIR] : []), join(home, '.openmausbot/task-workspaces')];
  return [...new Set(roots.map(root => {
    try { return realpathSync(root); } catch { return root; }
  }))];
}

export function isIgnored(dir, ignoreRoots) {
  return ignoreRoots.some(root => {
    const path = relative(resolve(root), resolve(dir));
    return path === '' || (path !== '..' && !path.startsWith(`..${sep}`) && !isAbsolute(path));
  });
}

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
  if (config.captureConversation !== undefined && typeof config.captureConversation !== 'boolean') throw new Error('invalid configuration');
  const captureConversation = config.captureConversation ?? true;
  if (config.ignoreRoots !== undefined && (!Array.isArray(config.ignoreRoots) || config.ignoreRoots.length > 32
      || config.ignoreRoots.some(root => typeof root !== 'string' || !isAbsolute(root) || /[\u0000-\u001f\u007f-\u009f]/.test(root)))) throw new Error('invalid configuration');
  const ignoreRoots = config.ignoreRoots === undefined ? defaultIgnoreRoots(env, env.HOME || homedir())
    : [...new Set(config.ignoreRoots.map(root => { try { return realpathSync(root); } catch { return root; } }))];
  const targets = config.skillTargets;
  if (targets !== undefined && (!targets || typeof targets !== 'object' || Array.isArray(targets)
      || Object.keys(targets).some(key => !['user', 'project'].includes(key))
      || Object.entries(targets).some(([scope, dirs]) => !Array.isArray(dirs) || dirs.length > 4 || dirs.some(dir =>
        typeof dir !== 'string' || !dir || /[\u0000-\u001f\u007f-\u009f]/.test(dir)
        || (scope === 'user' ? !isAbsolute(dir) : isAbsolute(dir) || dir.split(sep).includes('..')))))) throw new Error('invalid configuration');
  const home = env.HOME || homedir();
  const skillTargets = { user: targets?.user ?? [join(home, '.claude/skills'), join(home, '.omp/agent/managed-skills')],
    project: targets?.project ?? ['.claude/skills'] };
  const runtimeEnv = config.runtimeEnv ?? {};
  const options = new Set(['MAX_CONTENT_BYTES', 'MAX_PENDING', 'MAX_CANDIDATES', 'CONTEXT_BYTES', 'EMBEDDING_DIMENSIONS', 'DREAM_BATCH', 'TREE_MAX_CHILDREN', 'TREE_MAX_DEPTH', 'TREE_SPLIT_SOURCES', 'TREE_MAP_BYTES', 'CURATION_BATCH', 'IDLE_MS', 'SESSION_IDLE_MS', 'TICK_MS', 'RETENTION_MS'].map(name => `LAMBDA_MEMORY_${name}`));
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
  return { ...config, mode, runtimeEnv, webPort, webLeaseMs, captureConversation, ignoreRoots, skillTargets, configPath: path };
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

async function repositoryBinding(directory) {
  const root = await gitAsync(directory, ['rev-parse', '--show-toplevel']);
  if (!root) return null;
  const canonical = await realpath(root);
  const remote = await gitAsync(canonical, ['remote', 'get-url', 'upstream']) || await gitAsync(canonical, ['remote', 'get-url', 'origin']);
  return projectIdentity(canonical, remote);
}

async function markerBinding(directory, stop, fallback) {
  for (let current = directory; ; current = dirname(current)) {
    for (const marker of ['CLAUDE.md', 'AGENTS.md', '.lambda-memory']) {
      try { await lstat(join(current, marker)); return projectIdentity(current, ''); }
      catch (error) { if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error; }
    }
    if (current === stop || dirname(current) === current) return projectIdentity(fallback, '');
  }
}

async function canonicalDirectory(dir) {
  if (typeof dir !== 'string' || !isAbsolute(dir)) throw new Error('absolute project directory required');
  const canonical = await realpath(dir);
  if (!(await stat(canonical)).isDirectory()) throw new Error('project directory required');
  return canonical;
}

export async function resolveCallBindingAsync(cwd, { sessionRoot, ignoreRoots }) {
  const dir = await canonicalDirectory(cwd);
  if (isIgnored(dir, ignoreRoots)) return null;
  const repository = await repositoryBinding(dir);
  if (repository) return repository;
  let root;
  if (typeof sessionRoot === 'string' && isAbsolute(sessionRoot)) {
    try { root = await canonicalDirectory(sessionRoot); } catch { /* unusable session root */ }
  }
  if (root && isIgnored(dir, [root])) return markerBinding(dir, root, root);
  return markerBinding(dir, null, dir);
}

export async function resolveDirectoryIdentityAsync(dir, { ignoreRoots }) {
  let directory;
  try { directory = await canonicalDirectory(dir); }
  catch (error) {
    if (error.code === 'ENOENT' || error.code === 'ENOTDIR') return { action: 'missing' };
    throw error;
  }
  if (isIgnored(directory, ignoreRoots)) return { action: 'drop' };
  const binding = await repositoryBinding(directory) || await markerBinding(directory, null, directory);
  return { action: binding.cwd === directory ? 'keep' : 'merge', binding };
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

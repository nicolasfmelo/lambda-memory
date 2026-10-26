import { createHash } from 'node:crypto';

export const READ_ONLY_TOOLS = new Set(['read', 'grep', 'find', 'ls', 'Read', 'Grep', 'Glob', 'WebFetch', 'WebSearch']);

const SHELL_TOOLS = new Set(['bash', 'eval', 'Bash']);
const PATH_TOOLS = new Set(['edit', 'write', 'Edit', 'MultiEdit', 'Write', 'NotebookEdit']);
const TASK_TOOLS = new Set(['task', 'Task', 'Agent']);

const LIMITS = { intent: 160, summary: 200, errorHead: 200, content: 1024, tool: 128 };
const INPUT_CAP = 20000;
const REDACTED = '[REDACTED]';

const SENSITIVE_NAME = '[\\w.-]*(?:key|token|secret|passw(?:or)?d|credential|auth)[\\w.-]*';
const VALUE = '(?:(?:Bearer|Basic)\\s+)?(?:"[^"]*"|\'[^\']*\'|[^\\s,;&"\']+)';
const ASSIGNMENT = new RegExp(`(["']?)(${SENSITIVE_NAME})(["']?\\s*[:=]\\s*)${VALUE}`, 'gi');
const FLAG_VALUE = new RegExp(`(--${SENSITIVE_NAME}\\s+)(?!-)${VALUE}`, 'gi');
const USERINFO = /([A-Za-z][A-Za-z0-9+.-]*:\/\/)[^\s/@:]+:[^\s/@]+@/g;
const AUTH_SCHEME = /\b(Bearer|Basic)\s+[^\s"']+/g;
const KEY_PREFIX = /(?<![A-Za-z0-9])(?:sk-(?:ant-|or-)?|ghp_|gho_|github_pat_|glpat-|xox[abp]-)[A-Za-z0-9_-]{6,}|(?<![A-Za-z0-9])AKIA[A-Z0-9]{8,}|(?<![A-Za-z0-9])AIza[A-Za-z0-9_-]{8,}/g;
const LONG_RUN = /[A-Za-z0-9+/]{32,}={0,2}/g;

// Short password flags are only secrets for a few commands; elsewhere -p means
// "parents", "package", "port" and so on, so each form is anchored to its command.
const QUOTED_OR_WORD = '(?:"[^"]*"|\'[^\']*\'|\\S+)';
const BIN = '(?:\\S*/)?';
const DB_CLIENT = new RegExp(`^\\s*${BIN}(?:mysql|mysqldump|mariadb|mariadb-dump)(?=\\s)`);
const DB_GLUED_PASSWORD = new RegExp(`(\\s-p)(?=\\S)${QUOTED_OR_WORD}`, 'g');
const REGISTRY_LOGIN = new RegExp(`^\\s*${BIN}(?:docker|podman|nerdctl|helm)\\s+(?:(?:-\\S+\\s+(?:(?!login\\b)[^-\\s]\\S*\\s+)?|registry\\s+))*login(?=\\s|$)`);
const LOGIN_PASSWORD = new RegExp(`(\\s-p(?:\\s+|=)?)(?!-)${QUOTED_OR_WORD}`, 'g');
// Only -p inside sshpass's own options: a later `ssh -p 22` is a port.
const SSHPASS_PASSWORD = new RegExp(`^(\\s*${BIN}sshpass(?:\\s+(?:-[ehv]|-[fdP]\\s+\\S+))*\\s+-p(?:\\s+|(?=\\S)))(?!-)${QUOTED_OR_WORD}`);

function redactPasswordFlags(text) {
  return text.split('\n').map(line => {
    if (DB_CLIENT.test(line)) return line.replace(DB_GLUED_PASSWORD, `$1${REDACTED}`);
    const login = REGISTRY_LOGIN.exec(line);
    if (login) {
      const head = login[0];
      return head + line.slice(head.length).replace(LOGIN_PASSWORD, `$1${REDACTED}`);
    }
    return line.replace(SSHPASS_PASSWORD, `$1${REDACTED}`);
  }).join('\n');
}

export function shortHash(value) {
  return createHash('sha256').update(String(value)).digest('hex').slice(0, 16);
}

export function sourceId(sessionId, callId) {
  return `${shortHash(sessionId)}/${shortHash(callId)}`;
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function normalizePaths(text, prefix, replacement) {
  if (typeof prefix !== 'string') return text;
  const clean = prefix.replace(/\/+$/, '');
  if (!clean.startsWith('/') || clean.length < 2) return text;
  return text.replace(new RegExp(`${escapeRegExp(clean)}(?![\\w.~-])`, 'g'), replacement);
}

// A slash-bearing run is kept when it looks like a path: base64 payloads mix
// digits with upper and lower case, ordinary paths rarely do.
function longRunIsSecret(run) {
  if (!run.includes('/')) return true;
  return /\d/.test(run) && /[A-Z]/.test(run) && /[a-z]/.test(run);
}

export function sanitize(text, { home, root } = {}) {
  let out = String(text ?? '');
  out = normalizePaths(out, root, '.');
  out = normalizePaths(out, home, '~');
  out = redactPasswordFlags(out);
  out = out.replace(USERINFO, `$1${REDACTED}@`);
  out = out.replace(AUTH_SCHEME, `$1 ${REDACTED}`);
  out = out.replace(ASSIGNMENT, `$1$2$3${REDACTED}`);
  out = out.replace(FLAG_VALUE, `$1${REDACTED}`);
  out = out.replace(KEY_PREFIX, REDACTED);
  out = out.replace(LONG_RUN, run => (longRunIsSecret(run) ? REDACTED : run));
  return out;
}

function chars(text) {
  return Array.from(text);
}

function truncate(text, max) {
  const points = chars(text);
  return points.length <= max ? text : points.slice(0, max).join('');
}

function oneLine(text) {
  return text.replace(/[\u0000-\u001f\u007f]+/g, ' ').replace(/\s+/g, ' ').trim();
}

function firstLine(text) {
  for (const line of text.split('\n')) {
    const clean = oneLine(line);
    if (clean) return clean;
  }
  return '';
}

function field(value, opts, max, { multiline = false } = {}) {
  if (typeof value !== 'string' || !value) return '';
  const clean = sanitize(value.slice(0, INPUT_CAP), opts);
  return truncate(multiline ? firstLine(clean) : oneLine(clean), max);
}

function pickString(input, names) {
  for (const name of names) {
    if (typeof input?.[name] === 'string' && input[name]) return input[name];
  }
  return '';
}

function summaryFor(tool, input, opts) {
  if (SHELL_TOOLS.has(tool)) {
    return field(pickString(input, ['command', 'code']), opts, LIMITS.summary, { multiline: true });
  }
  if (PATH_TOOLS.has(tool)) {
    return field(pickString(input, ['path', 'file_path', 'notebook_path']), opts, LIMITS.summary);
  }
  return '';
}

function fit(body) {
  const keys = ['summary', 'error_head', 'intent'];
  let content = JSON.stringify(body);
  while (Buffer.byteLength(content) > LIMITS.content) {
    const key = keys
      .filter(name => typeof body[name] === 'string' && body[name])
      .sort((a, b) => chars(body[b]).length - chars(body[a]).length)[0];
    if (!key) return null;
    const points = chars(body[key]);
    body[key] = points.slice(0, points.length - Math.max(1, Math.floor(points.length / 8))).join('');
    content = JSON.stringify(body);
  }
  return content;
}

export function buildEpisode({ agent, tool, input, isError, errorText, sessionId, callId, seq, home, root }) {
  if (typeof tool !== 'string' || typeof sessionId !== 'string' || !sessionId || typeof callId !== 'string' || !callId) return null;
  const failed = isError === true;
  const captured = SHELL_TOOLS.has(tool) || PATH_TOOLS.has(tool) || TASK_TOOLS.has(tool);
  if (!failed && !captured) return null;
  const name = tool.slice(0, LIMITS.tool).replace(/[^A-Za-z0-9_.:-]/g, '_');
  if (!/^[A-Za-z]/.test(name)) return null;
  const opts = { home, root };
  const intentSource = agent === 'claude-code' ? input?.description : input?.i;
  const errorHead = failed ? field(errorText, opts, LIMITS.errorHead, { multiline: true }) : '';
  const body = {
    v: 1,
    tool: name,
    intent: field(intentSource, opts, LIMITS.intent),
    summary: summaryFor(tool, input, opts),
    error_head: errorHead || null,
    session: shortHash(sessionId),
    seq: Number.isSafeInteger(seq) && seq >= 0 ? seq : 0,
  };
  const content = fit(body);
  if (content === null) return null;
  return { source: sourceId(sessionId, callId), kind: 'action', outcome: failed ? 'failed' : 'succeeded', content };
}

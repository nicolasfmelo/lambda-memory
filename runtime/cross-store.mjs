import { createHash } from 'node:crypto';
import { realpath } from 'node:fs/promises';
import { basename, isAbsolute, relative, sep } from 'node:path';
import { isIgnored } from './project-binding.mjs';
import { MAX_RESPONSE_BYTES } from './framing.mjs';

export const RECALL_SIDE_DEADLINE_MS = 2500;
export const BRIEF_SIDE_DEADLINE_MS = 1200;
export const CHILD_LEASE_MS = 600000;
export const DRAIN_BACKOFF_MS = 300000;
export const SKILL_TOOLS = new Set(['memory_skill_candidates', 'memory_skill_candidate', 'memory_skill_decide']);
export const SERVICE_ONLY_TOOLS = new Set(['memory_forwards', 'memory_forward_done', 'memory_overview', 'memory_nodes', ...SKILL_TOOLS]);
const hash = value => createHash('sha256').update(value).digest('hex');

export function copyId(project, source) {
  const prefix = `promote/${hash(project).slice(0, 16)}/`;
  const id = prefix + source;
  return Buffer.byteLength(id) <= 512 ? id : `${prefix}h-${hash(source)}`;
}

export function resultBody(result) {
  if (!result || result.isError) return null;
  try {
    const body = JSON.parse(result.content.filter(part => part.type === 'text').map(part => part.text).join(''));
    return body && typeof body === 'object' && !Array.isArray(body) ? body : null;
  } catch { return null; }
}
const textResult = body => ({ content: [{ type: 'text', text: JSON.stringify(body) }], isError: false });

// Never split a UTF-8 code point at the byte budget boundary.
function cut(text, bytes) {
  const buffer = Buffer.from(text);
  if (buffer.length <= bytes) return text;
  let end = bytes;
  while (end > 0 && (buffer[end] & 0xc0) === 0x80) end -= 1;
  return buffer.subarray(0, end).toString('utf8');
}

/** Sides are MCP results (or null on failure); workspace entries are {cwd, result}. */
export function mergeRecall(projectResult, userResult, workspace = [], budgetBytes) {
  const project = resultBody(projectResult);
  if (!project) return projectResult;
  const user = resultBody(userResult);
  const children = workspace.map(child => ({ ...child, body: resultBody(child.result) })).filter(child => child.body);
  let context = typeof project.context === 'string' ? project.context : '';
  let truncated = Boolean(project.truncated || user?.truncated || children.some(child => child.body.truncated));
  const bounded = Number.isSafeInteger(budgetBytes) && budgetBytes >= 0;
  const append = section => {
    const remaining = bounded ? Math.max(0, budgetBytes - Buffer.byteLength(context)) : Infinity;
    const kept = bounded ? cut(section, remaining) : section;
    if (kept !== section) truncated = true;
    context += kept;
  };
  if (bounded && Buffer.byteLength(context) > budgetBytes) {
    context = cut(context, budgetBytes); truncated = true;
  }
  if (user?.context) append(`\n\nUser-level memory:\n${user.context}`);
  for (const child of children) if (child.body.context) append(`\n\nWorkspace memory (${basename(child.cwd)}):\n${child.body.context}`);
  const hits = (body, scope) => (Array.isArray(body?.hits) ? body.hits : []).map(hit => ({ ...hit, scope }));
  const projectHits = hits(project, 'project'), userHits = hits(user, 'user');
  const body = { context, mode: project.mode, truncated,
    hits: [...projectHits, ...userHits, ...children.flatMap(child => hits(child.body, 'workspace'))] };
  const tooLarge = () => Buffer.byteLength(JSON.stringify(textResult(body))) > MAX_RESPONSE_BYTES - 65536;
  if (tooLarge()) { body.hits = [...projectHits, ...userHits]; body.truncated = true; }
  if (tooLarge()) { body.hits = projectHits; body.truncated = true; }
  return tooLarge() ? projectResult : textResult(body);
}

export function mergeBrief(projectResult, userResult, workspace = []) {
  const project = resultBody(projectResult);
  if (!project) return projectResult;
  const body = { ...project }, user = resultBody(userResult);
  body.skill_drafts_pending = (project.skill_drafts_pending ?? 0) + (user?.skill_drafts_pending ?? 0);
  if (user?.roots?.length) body.user = { roots: user.roots };
  const children = workspace.flatMap(child => {
    const result = resultBody(child.result);
    return result?.roots?.length ? [{ project: child.project, name: basename(child.cwd), roots: result.roots }] : [];
  });
  if (children.length) body.workspace = children;
  return textResult(body);
}

export async function workspaceChildren(binding, userProject, entries, ignoreRoots = [], canonicalize = realpath) {
  let root;
  try { root = await canonicalize(binding.cwd); } catch { return []; }
  const children = await Promise.all(entries.map(async entry => {
    if (entry.project === binding.project || entry.project === userProject) return null;
    try {
      const cwd = await canonicalize(entry.cwd), path = relative(root, cwd);
      if (!path || path === '..' || path.startsWith(`..${sep}`) || isAbsolute(path) || isIgnored(cwd, ignoreRoots)) return null;
      return { ...entry, cwd };
    } catch { return null; }
  }));
  return children.filter(Boolean).sort((a, b) => b.last_seen_ms - a.last_seen_ms).slice(0, 4);
}

/** Returns a target and stripped arguments, or throws a definitive local parameter error. */
export function routeTool(name, args, project, user, { operator = false } = {}) {
  if (SERVICE_ONLY_TOOLS.has(name) && !(operator && SKILL_TOOLS.has(name))) throw Object.assign(new Error('unknown tool'), { rpcCode: -32602 });
  if (project === user) return { target: 'project', args };
  if (name === 'memory_capture' && Object.hasOwn(args, 'scope')) {
    const { scope, ...rest } = args;
    if (!['project', 'user'].includes(scope)) throw Object.assign(new Error('invalid params'), { rpcCode: -32602 });
    if (scope === 'user') return { target: 'user', args: { ...rest, project: user } };
    return { target: 'project', args: rest };
  }
  const only = id => Array.isArray(args.projects) && args.projects.length === 1 && args.projects[0] === id;
  if (args.project === user || (name === 'memory_recall' && only(user))) return { target: 'user', args };
  if ((name === 'memory_recall' && (args.projects === undefined || only(project)))
      || (name === 'memory_brief' && args.project === project)) return { target: 'fanout', args };
  return { target: 'project', args };
}

function failureText(result) {
  try { return JSON.parse(result.content.filter(part => part.type === 'text').map(part => part.text).join('')).error || ''; }
  catch { return ''; }
}

/** One ordered pass. Callers own worker leases, serialization, timers and backoff. */
export async function drainForwards({ project, user, callProject, callUser, stopped = () => false }) {
  const done = [], failed = [], uncertain = new Set();
  let backoff = false;
  try {
    if (stopped()) return { backoff };
    const queue = resultBody(await callProject('memory_forwards', { project, limit: 32 }));
    for (const forward of queue?.forwards || []) {
      if (stopped()) break;
      if (uncertain.has(forward.source_id)) continue;
      if (forward.action === 'capture' && !forward.source) { done.push(forward.forward_id); continue; }
      try {
        const source = copyId(project, forward.source_id);
        const result = forward.action === 'capture'
          ? await callUser('memory_capture', { project: user, source, kind: forward.source.kind, outcome: forward.source.outcome,
            origin: `promote://${project}/${forward.source_id}`, content: forward.source.content, recorded_at_ms: forward.source.recorded_at_ms })
          : await callUser('memory_purge', { project: user, sources: [source] });
        const message = result?.isError ? failureText(result) : '';
        if (message === 'pending capture capacity reached') { backoff = true; break; }
        if ((!result?.isError && (forward.action === 'purge' || ['saved_pending', 'already_saved_pending'].includes(resultBody(result)?.status)))
            || (forward.action === 'purge' && message === 'record not found')) done.push(forward.forward_id);
        else if (message.startsWith('invalid runtime input') || message.includes('lifecycle or source conflict')) failed.push(forward.forward_id);
        else uncertain.add(forward.source_id);
      } catch (failure) {
        if (failure?.rpcCode === -32602) failed.push(forward.forward_id);
        else uncertain.add(forward.source_id);
      }
    }
    if (!stopped() && (done.length || failed.length)) {
      await callProject('memory_forward_done', { project, forwards: done, ...(failed.length ? { failed } : {}) });
    }
  } catch { /* unavailable queues and uncertain acknowledgements stay pending */ }
  return { backoff };
}

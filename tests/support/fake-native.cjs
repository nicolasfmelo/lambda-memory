#!/usr/bin/env node
// Minimal native MCP server for daemon tests. `memory_crash` exits the process; read-only tools
// carry annotations.readOnlyHint like the real server; calls echo their name and arguments
// (an optional `delay_ms` argument delays the reply; `rpc_error_code` answers with that JSON-RPC error).
const { appendFileSync, existsSync, readFileSync, writeFileSync } = require('node:fs');
const { createInterface } = require('node:readline');
const TOOLS = [
  { name: 'memory_status', annotations: { readOnlyHint: true } },
  { name: 'memory_timeline', annotations: { readOnlyHint: true } },
  { name: 'memory_timeline_write', annotations: { readOnlyHint: false } },
  { name: 'memory_crash' },
  { name: 'memory_capture', annotations: { readOnlyHint: false } },
  { name: 'memory_propose', annotations: { readOnlyHint: false } },
  { name: 'memory_purge', annotations: { readOnlyHint: false, destructiveHint: true } },
];
// Opt-in behaviors for hook/importer tests; without these variables nothing below changes:
//   FAKE_NATIVE_BRIEF     JSON text returned by an extra read-only `memory_brief` tool
//   FAKE_NATIVE_RECALL    JSON text returned by an extra read-only `memory_recall` tool
//   FAKE_NATIVE_LOG       file that receives one JSON line per tools/call ({name, arguments, project, pid})
//   FAKE_NATIVE_STORE     file of captured sources; memory_capture answers saved_pending, then already_saved_pending
//   FAKE_NATIVE_REJECT    regex; a memory_capture whose content matches answers an isError result
//   FAKE_NATIVE_PENDING_AFTER  reject new sources after this many new saves in this process
//   FAKE_NATIVE_CONFLICT  source whose capture answers a lifecycle or source conflict
//   FAKE_NATIVE_FORWARDS  {forwards: [...]} for project workers; captures answer saved_pending
//   FAKE_NATIVE_RECALL_BY_PROJECT / FAKE_NATIVE_BRIEF_BY_PROJECT  JSON bodies keyed by project
//   FAKE_NATIVE_DELAY_BY_PROJECT / FAKE_NATIVE_ERROR_BY_PROJECT  delays / capture errors by project
//   FAKE_NATIVE_EXTRA_TOOLS  comma-separated extra tool names
//   FAKE_NATIVE_UI  enables canned UI-only read tools
const project = process.env.LAMBDA_MEMORY_PROJECT;
const recallByProject = process.env.FAKE_NATIVE_RECALL_BY_PROJECT && JSON.parse(process.env.FAKE_NATIVE_RECALL_BY_PROJECT);
const briefByProject = process.env.FAKE_NATIVE_BRIEF_BY_PROJECT && JSON.parse(process.env.FAKE_NATIVE_BRIEF_BY_PROJECT);
const delayByProject = JSON.parse(process.env.FAKE_NATIVE_DELAY_BY_PROJECT || '{}');
const errorByProject = JSON.parse(process.env.FAKE_NATIVE_ERROR_BY_PROJECT || '{}');
const forwards = process.env.FAKE_NATIVE_FORWARDS && JSON.parse(process.env.FAKE_NATIVE_FORWARDS);
const skills = process.env.FAKE_NATIVE_SKILLS;
if (process.env.FAKE_NATIVE_UI) for (const name of ['memory_node', 'memory_nodes', 'memory_overview']) TOOLS.push({ name, annotations: { readOnlyHint: true } });
if (skills) for (const name of ['memory_skill_candidates', 'memory_skill_candidate', 'memory_skill_decide']) TOOLS.push({ name });
const answerDelays = new Map();
let newSources = 0;
if (forwards && !project?.startsWith('user:')) TOOLS.push({ name: 'memory_forwards', annotations: { readOnlyHint: true } }, { name: 'memory_forward_done' });
for (const name of (process.env.FAKE_NATIVE_EXTRA_TOOLS || '').split(',').filter(Boolean)) {
  if (!TOOLS.some(tool => tool.name === name)) TOOLS.push({ name });
}
if (process.env.FAKE_NATIVE_BRIEF || briefByProject) TOOLS.push({ name: 'memory_brief', annotations: { readOnlyHint: true } });
if (process.env.FAKE_NATIVE_RECALL || recallByProject) TOOLS.push({ name: 'memory_recall', annotations: { readOnlyHint: true } });
const textResult = (id, body, isError = false) => ({ jsonrpc: '2.0', id, result: { content: [{ type: 'text', text: JSON.stringify(body) }], isError } });
const reply = (message) => {
  const delay = answerDelays.get(message.id) || 0;
  answerDelays.delete(message.id);
  const send = () => process.stdout.write(JSON.stringify(message) + '\n');
  if (delay) setTimeout(send, delay); else send();
};
// Slow shutdown: stay alive after stdin closes until the daemon's 1s deadline kills us.
process.stdin.on('end', () => setTimeout(() => process.exit(0), 5000));
createInterface({ input: process.stdin }).on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined) return;
  if (message.method === 'tools/call') answerDelays.set(message.id, Number(delayByProject[project]) || 0);
  if (message.method === 'tools/call' && process.env.FAKE_NATIVE_LOG) {
    appendFileSync(process.env.FAKE_NATIVE_LOG, JSON.stringify({ name: message.params?.name, arguments: message.params?.arguments ?? {}, project, pid: process.pid }) + '\n');
  }
  if (message.method === 'initialize') {
    reply({ jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'fake', version: '0' } } });
  } else if (message.method === 'tools/list') {
    reply({ jsonrpc: '2.0', id: message.id, result: { tools: TOOLS.map((tool) => ({ ...tool, inputSchema: { type: 'object' } })) } });
  } else if (process.env.FAKE_NATIVE_UI && ['memory_node', 'memory_nodes', 'memory_overview'].includes(message.params?.name)) {
    const name = message.params.name, args = message.params.arguments;
    reply(textResult(message.id, name === 'memory_node' ? { node: args.node, title: 'Title', path: ['Decisions', 'Topic'] } : { project: args.project, origin: `worker://${project}`, nodes: [] }));
  } else if (skills && ['memory_skill_candidates', 'memory_skill_candidate', 'memory_skill_decide'].includes(message.params?.name)) {
    const args = message.params.arguments, rows = JSON.parse(readFileSync(skills, 'utf8'));
    const row = rows.find(item => item.candidate_id === args.candidate_id);
    if (message.params.name === 'memory_skill_candidates') reply(textResult(message.id, { candidates: rows.map(({ draft, install_hashes, ...item }) => item) }));
    else if (!row) reply(textResult(message.id, { error: 'record not found' }, true));
    else if (message.params.name === 'memory_skill_decide' && (args.revision_id !== row.revision_id || args.decision === process.env.FAKE_NATIVE_SKILL_REJECT_DECISION)) {
      reply(textResult(message.id, { error: 'skill revision mismatch' }, true));
    } else {
      if (message.params.name === 'memory_skill_decide') {
        row.status = args.decision;
        if (args.decision === 'installed') { row.install_paths = args.install_paths; row.install_hashes = args.install_hashes; }
        writeFileSync(skills, JSON.stringify(rows));
      }
      reply(textResult(message.id, row));
    }
  } else if (message.params?.name === 'memory_forwards' && forwards && !project?.startsWith('user:')) {
    reply(textResult(message.id, forwards));
  } else if (message.params?.name === 'memory_recall' && recallByProject) {
    const body = recallByProject[project] ?? { context: '', mode: 'tree', truncated: false, hits: [] };
    reply(typeof body === 'string' ? { jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: body }], isError: false } } : textResult(message.id, body));
  } else if (message.params?.name === 'memory_brief' && briefByProject) {
    const body = briefByProject[project] ?? { project, mode: 'tree', roots: [], truncated: false };
    reply(typeof body === 'string' ? { jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: body }], isError: false } } : textResult(message.id, body));
  } else if (message.params?.name === 'memory_capture' && errorByProject[project]) {
    reply(textResult(message.id, { error: errorByProject[project] }, true));
  } else if (message.params?.name === 'memory_capture' && forwards) {
    reply(textResult(message.id, { source: message.params.arguments?.source, status: 'saved_pending' }));
  } else if (message.params?.name === 'memory_brief' && process.env.FAKE_NATIVE_BRIEF) {
    reply({ jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: process.env.FAKE_NATIVE_BRIEF }], isError: false } });
  } else if (message.params?.name === 'memory_recall' && process.env.FAKE_NATIVE_RECALL) {
    reply({ jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: process.env.FAKE_NATIVE_RECALL }], isError: false } });
  } else if (message.params?.name === 'memory_capture' && process.env.FAKE_NATIVE_REJECT
      && new RegExp(process.env.FAKE_NATIVE_REJECT).test(String(message.params.arguments?.content))) {
    reply(textResult(message.id, { error: 'invalid action episode' }, true));
  } else if (message.params?.name === 'memory_capture' && process.env.FAKE_NATIVE_CONFLICT
      && process.env.FAKE_NATIVE_CONFLICT === message.params.arguments?.source) {
    reply(textResult(message.id, { error: 'lifecycle or source conflict' }, true));
  } else if (message.params?.name === 'memory_capture' && process.env.FAKE_NATIVE_STORE) {
    const store = process.env.FAKE_NATIVE_STORE;
    const source = message.params.arguments?.source;
    const known = existsSync(store) && readFileSync(store, 'utf8').split('\n').includes(source);
    if (!known && process.env.FAKE_NATIVE_PENDING_AFTER !== undefined && newSources >= Number(process.env.FAKE_NATIVE_PENDING_AFTER)) {
      reply(textResult(message.id, { error: 'pending capture capacity reached' }, true));
    } else {
      if (!known) { appendFileSync(store, `${source}\n`); newSources += 1; }
      reply(textResult(message.id, { source, status: known ? 'already_saved_pending' : 'saved_pending' }));
    }
  } else if (message.params?.name === 'memory_crash') {
    process.exit(3);
  } else if (message.params?.arguments?.rpc_error_code !== undefined) {
    reply({ jsonrpc: '2.0', id: message.id, error: { code: message.params.arguments.rpc_error_code, message: 'fake rpc error' } });
  } else {
    const text = JSON.stringify({ name: message.params?.name, arguments: message.params?.arguments ?? {} });
    const delay = Number(message.params?.arguments?.delay_ms) || 0; // simulates a slow native call
    setTimeout(() => reply({ jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text }], isError: false } }), delay);
  }
});

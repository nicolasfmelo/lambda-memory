#!/usr/bin/env node
// Minimal native MCP server for daemon tests. `memory_crash` exits the process; read-only tools
// carry annotations.readOnlyHint like the real server; calls echo their name and arguments
// (an optional `delay_ms` argument delays the reply; `rpc_error_code` answers with that JSON-RPC error).
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
const reply = (message) => process.stdout.write(JSON.stringify(message) + '\n');
// Slow shutdown: stay alive after stdin closes until the daemon's 1s deadline kills us.
process.stdin.on('end', () => setTimeout(() => process.exit(0), 5000));
createInterface({ input: process.stdin }).on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined) return;
  if (message.method === 'initialize') {
    reply({ jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'fake', version: '0' } } });
  } else if (message.method === 'tools/list') {
    reply({ jsonrpc: '2.0', id: message.id, result: { tools: TOOLS.map((tool) => ({ ...tool, inputSchema: { type: 'object' } })) } });
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

import { createConnection } from 'node:net';
import { loadConfig } from './project-binding.mjs';
import { frames, writeFrame, writeRawFrame, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES } from './framing.mjs';

let socket;
try {
  if (process.argv.length !== 3 || process.argv[2] !== '--serve-mcp') throw new Error('use --serve-mcp');
  const config = loadConfig();
  socket = createConnection({ path: config.socketPath, allowHalfOpen: true });
  socket.on('error', () => {});
  const interrupt = () => { socket.destroy(); process.stdin.destroy(); };
  process.once('SIGTERM', interrupt); process.once('SIGINT', interrupt);
  process.stdout.on('error', interrupt);
  await new Promise((resolve, reject) => { socket.once('connect', resolve); socket.once('error', reject); });
  await writeFrame(socket, { cwd: process.cwd(), token: config.token }, MAX_REQUEST_BYTES);
  const responses = frames(socket, MAX_RESPONSE_BYTES);
  const acknowledged = await responses.next();
  // The service resolves the binding; reuse it instead of re-running git here.
  const binding = acknowledged.done ? null : JSON.parse(acknowledged.value);
  if (binding?.ready !== true || typeof binding.project !== 'string') throw new Error('service not ready');
  const listRequests = new Set();
  const output = (async () => {
    try {
      for await (const text of responses) {
        // Only replies to tools/list are rewritten; everything else passes through unparsed.
        const parsed = listRequests.size ? JSON.parse(text) : null;
        const response = parsed && listRequests.delete(parsed.id) ? parsed : null;
        if (Array.isArray(response?.result?.tools)) {
          for (const tool of response.result.tools) {
            tool.description = `${tool.description ?? ''} Current authorized project: ${binding.project}. Do not broaden project scope or automatically retry an indeterminate write.`;
          }
          await writeFrame(process.stdout, response, MAX_RESPONSE_BYTES);
        } else {
          await writeRawFrame(process.stdout, text, MAX_RESPONSE_BYTES);
        }
      }
    }
    finally { process.stdin.destroy(); }
  })();
  const input = (async () => {
    try {
      for await (const original of frames(process.stdin, MAX_REQUEST_BYTES)) {
        let text = original;
        try {
          const request = JSON.parse(text);
          if (request?.method === 'tools/list' && (typeof request.id === 'string' || typeof request.id === 'number')) listRequests.add(request.id);
          if (request?.method === 'initialize' && request.params && typeof request.params === 'object' && !Array.isArray(request.params)) {
            const meta = request.params._meta;
            if (meta === undefined || (meta && typeof meta === 'object' && !Array.isArray(meta) && !Object.hasOwn(meta, 'lambdaMemoryToken'))) {
              request.params._meta = { ...meta, lambdaMemoryToken: config.token };
              text = JSON.stringify(request);
            }
          }
        } catch { /* malformed JSON belongs to the MCP server, not diagnostics */ }
        await writeRawFrame(socket, text, MAX_REQUEST_BYTES);
      }
      socket.end();
    } catch (failure) { socket.destroy(); throw failure; }
  })();
  await Promise.all([input, output]);
  process.off('SIGTERM', interrupt); process.off('SIGINT', interrupt);
} catch {
  socket?.destroy(); process.stdin.destroy();
  console.error('Lambda Memory connection failed; no write is confirmed. Check the local service and private configuration.');
  process.exitCode = 2;
}

import { isAbsolute } from 'node:path';

export const origin = 'codex://action';
export const counterPrefix = 'codex';

export function patchPaths(patchText) {
  if (typeof patchText !== 'string') return [];
  const paths = [];
  let preceding = null;
  for (const line of patchText.split('\n')) {
    const header = /^\*\*\* (Add|Update|Delete) File: (.+)$/.exec(line);
    if (header) {
      preceding = { path: header[2], op: header[1].toLowerCase() };
      paths.push(preceding);
      continue;
    }
    const move = /^\*\*\* Move to: (.+)$/.exec(line);
    if (move && preceding?.op === 'update') {
      preceding.path = move[1];
      preceding = null;
    }
  }
  const seen = new Set();
  return paths.filter(({ path }) => {
    if (seen.has(path)) return false;
    seen.add(path);
    return true;
  }).slice(0, 8);
}

export function calls(input) {
  if (!input || typeof input !== 'object' || Array.isArray(input)
    || input.hook_event_name !== 'PostToolUse' || Object.hasOwn(input, 'agent_id')
    || typeof input.session_id !== 'string' || !input.session_id
    || typeof input.tool_use_id !== 'string' || !input.tool_use_id
    || typeof input.cwd !== 'string' || !isAbsolute(input.cwd)) return [];
  const common = {
    sessionId: input.session_id, cwd: input.cwd, tool: input.tool_name,
    intent: '', outcome: 'unknown', errorText: '',
  };
  const command = input.tool_input?.command;
  if (input.tool_name === 'Bash' && typeof command === 'string') {
    return [{ ...common, callId: input.tool_use_id, kind: 'shell', summary: command }];
  }
  if (input.tool_name === 'apply_patch') {
    return patchPaths(command).map(({ path, op }, index) => ({
      ...common, callId: `${input.tool_use_id}#${index}`, kind: op === 'add' ? 'write' : 'edit', summary: path,
    }));
  }
  return [];
}

export function sessionCwd(input) {
  if (input?.hook_event_name !== 'SessionStart' || typeof input.cwd !== 'string' || !isAbsolute(input.cwd)) {
    throw new Error('SessionStart and absolute cwd required');
  }
  return input.cwd;
}

export function sessionContext(message) {
  return JSON.stringify({ hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: message } });
}

function exchangeInput(input, event) {
  if (!input || typeof input !== 'object' || Array.isArray(input)
    || input.hook_event_name !== event || Object.hasOwn(input, 'agent_id')
    || typeof input.session_id !== 'string' || !input.session_id
    || typeof input.cwd !== 'string' || !isAbsolute(input.cwd)) return null;
  return { sessionId: input.session_id, turnKey: typeof input.turn_id === 'string' ? input.turn_id : null, cwd: input.cwd };
}

export function exchangePrompt(input) {
  const common = exchangeInput(input, 'UserPromptSubmit');
  if (!common || typeof input.prompt !== 'string' || /^\/\S+\s*$/.test(input.prompt)) return null;
  return { ...common, prompt: input.prompt };
}

export function exchangeStop(input) {
  const common = exchangeInput(input, 'Stop');
  if (!common || input.stop_hook_active === true || (input.last_assistant_message != null && typeof input.last_assistant_message !== 'string')) return null;
  return { ...common, reply: input.last_assistant_message ?? '' };
}

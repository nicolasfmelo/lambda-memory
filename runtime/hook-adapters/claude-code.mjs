import { isAbsolute } from 'node:path';
import { describeClaudeCode } from './tool-kinds.mjs';

export const origin = 'claude-code://action';
export const counterPrefix = 'cc';

function responseText(response) {
  if (typeof response === 'string') return response;
  if (!response || typeof response !== 'object') return '';
  for (const key of ['error', 'stderr', 'content', 'message', 'stdout']) {
    const value = response[key];
    if (typeof value === 'string' && value.trim()) return value;
    if (Array.isArray(value)) {
      const text = value.map(part => (typeof part?.text === 'string' ? part.text : '')).filter(Boolean).join('\n');
      if (text.trim()) return text;
    }
  }
  return '';
}

export function calls(input) {
  if (!input || typeof input !== 'object' || Array.isArray(input)) return [];
  if (typeof input.session_id !== 'string' || !input.session_id || typeof input.tool_use_id !== 'string' || !input.tool_use_id) return [];
  const failureEvent = input.hook_event_name === 'PostToolUseFailure';
  if (failureEvent && input.is_interrupt === true) return [];
  const failed = failureEvent || input.tool_response?.is_error === true || input.tool_response?.success === false;
  return [{
    sessionId: input.session_id, callId: input.tool_use_id, cwd: input.cwd, tool: input.tool_name,
    ...describeClaudeCode(input.tool_name, input.tool_input), outcome: failed ? 'failed' : 'succeeded',
    errorText: !failed ? '' : failureEvent ? (typeof input.error === 'string' ? input.error : '') : responseText(input.tool_response),
  }];
}

export function sessionCwd(input) {
  if (typeof input?.cwd !== 'string' || !isAbsolute(input.cwd)) throw new Error('absolute cwd required');
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
  return { sessionId: input.session_id, turnKey: typeof input.prompt_id === 'string' ? input.prompt_id : null, cwd: input.cwd };
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

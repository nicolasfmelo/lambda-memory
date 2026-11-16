import { sanitize, shortHash } from './action-episode.mjs';

export const PROMPT_CHARS = 2000;
export const REPLY_CHARS = 4000;

export function turnText(text, { home, root }, limit) {
  return Array.from(sanitize(text, { home, root })).slice(0, limit).join('');
}

export function exchangeSource(sessionId, turnKey, prompt) {
  return `${shortHash(sessionId)}/turn-${shortHash(turnKey ?? prompt)}`;
}

export function exchangeContent(prompt, reply) {
  const body = { v: 1, prompt, reply };
  let content = JSON.stringify(body);
  for (const key of ['reply', 'prompt']) {
    if (Buffer.byteLength(content) <= 8192) break;
    const chars = Array.from(body[key]);
    let low = 0, high = chars.length;
    while (Buffer.byteLength(content) > 8192 && low < high) {
      const mid = Math.floor((low + high) / 2);
      body[key] = chars.slice(0, mid).join('');
      const candidate = JSON.stringify(body);
      if (Buffer.byteLength(candidate) <= 8192) low = mid + 1;
      else high = mid;
    }
    if (low < chars.length) {
      body[key] = chars.slice(0, Math.max(0, low - 1)).join('');
      content = JSON.stringify(body);
    }
  }
  return content;
}

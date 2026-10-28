const OMP_KINDS = { bash: 'shell', eval: 'shell', edit: 'edit', write: 'write', task: 'task' };
const CLAUDE_KINDS = { Bash: 'shell', Edit: 'edit', MultiEdit: 'edit', NotebookEdit: 'edit', Write: 'write', Task: 'task', Agent: 'task' };

function pickString(input, names) {
  for (const name of names) {
    if (typeof input?.[name] === 'string' && input[name]) return input[name];
  }
  return '';
}

function describe(tool, input, kinds, intentField) {
  const kind = Object.hasOwn(kinds, tool) ? kinds[tool] : 'other';
  const summary = kind === 'shell' ? pickString(input, ['command', 'code'])
    : kind === 'edit' || kind === 'write' ? pickString(input, ['path', 'file_path', 'notebook_path']) : '';
  return { kind, summary, intent: typeof input?.[intentField] === 'string' ? input[intentField] : '' };
}

export function describeOmp(tool, input) {
  return describe(tool, input, OMP_KINDS, 'i');
}

export function describeClaudeCode(tool, input) {
  return describe(tool, input, CLAUDE_KINDS, 'description');
}

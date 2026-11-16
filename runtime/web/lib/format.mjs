export const fmtTime = ms => ms == null ? '—' : new Date(ms).toLocaleString();
export function fmtRelative(ms) {
  if (ms == null)
    return '—';
  const days = Math.round((ms - Date.now()) / 86400000);
  return new Intl.RelativeTimeFormat(undefined, {numeric: 'auto'}).format(days, 'day');
}
export function fmtDay(day) {
  const d = typeof day === 'string' && /^\d{4}-\d\d-\d\d$/.test(day)
    ? new Date(`${day}T12:00:00`)
    : new Date(day);
  return Number.isFinite(d.getTime())
    ? d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
    : '—';
}
export function clip(text, bytes) {
  let out = '';
  let n = 0;
  for (const c of String(text ?? '')) {
    const size = new TextEncoder().encode(c).length;
    if (n + size > bytes)
      break;
    out += c;
    n += size;
  }
  return out;
}
export const projectLabel = project =>
  project === '@user' ? 'You' : (project?.split('/').filter(Boolean).pop() || project || '');

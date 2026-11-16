const pages = new Set([
  'map', 'type', 'memory', 'flat', 'flat-memory', 'skills', 'review', 'timeline', 'entry',
  'activity'
]);
const ids = new Set(['type', 'memory', 'flat', 'flat-memory', 'entry']);
export function parse(hash) {
  const parts = hash.replace(/^#\/?/, '').split('/');
  if (parts[0] !== 'p' || !parts[1])
    return {page: 'projects'};
  try {
    const project = decodeURIComponent(parts[1]);
    const page = pages.has(parts[2]) ? parts[2] : 'map';
    const id = ids.has(page) && parts[3] ? decodeURIComponent(parts[3]) : undefined;
    const state = page === 'review' && parts[3] ? decodeURIComponent(parts[3]) : undefined;
    return {page, project, id, state};
  } catch {
    return {page: 'projects'};
  }
}
export function href(route) {
  if (route.page === 'projects' || !route.project)
    return '#/';
  const extra = route.page === 'review' ? route.state : route.id;
  return `#/p/${encodeURIComponent(route.project)}/${route.page || 'map'}${
      extra === undefined ? '' : `/${encodeURIComponent(extra)}`}`;
}

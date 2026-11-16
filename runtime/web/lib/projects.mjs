// Only the newest request may publish; callers of superseded requests await the newest result.
export function createProjectRegistry({ load, onUpdate }) {
  let sequence = 0;
  let latest;

  function refresh() {
    const request = ++sequence;
    async function fetchProjects() {
      let update;
      try {
        const result = await load();
        update = { projects: result.projects || [], error: null };
      } catch (error) {
        update = { error };
      }
      if (request !== sequence) {
        return latest;
      }
      onUpdate(update);
      return update;
    }
    latest = fetchProjects();
    return latest;
  }
  return { refresh };
}

export function projectTarget(projects, remembered) {
  if (remembered && remembered !== '@user' && projects.some(p => p.project === remembered)) {
    return remembered;
  }
  return projects.find(p => p.project !== '@user')?.project;
}

export function listenForVisible(doc, refresh) {
  function onVisibilityChange() {
    if (!doc.hidden) {
      refresh();
    }
  }
  doc.addEventListener('visibilitychange', onVisibilityChange);
  return () => doc.removeEventListener('visibilitychange', onVisibilityChange);
}

import { html, render, useEffect, useMemo, useRef, useState } from './vendor/htm-preact-3.1.1.mjs';
import { createApi } from './lib/api.mjs';
import { parse, href } from './lib/router.mjs';
import { projectLabel } from './lib/format.mjs';
import { createProjectRegistry, listenForVisible } from './lib/projects.mjs';
import { ApiContext, NeighborsContext } from './components/nav.mjs';
import { Shell } from './components/shell.mjs';
import { Empty, ErrorNote, Loading, NeedsStart } from './components/states.mjs';
import { MapView } from './views/map.mjs';
import { GridView } from './views/grid.mjs';
import { MemoryModal } from './views/memory-modal.mjs';
import { SkillsView } from './views/skills.mjs';
import { ReviewView } from './views/review.mjs';
import { TimelineView } from './views/timeline.mjs';
import { ActivityView } from './views/activity.mjs';

const api = createApi({
  fetchImpl: window.fetch.bind(window),
  csrf: document.querySelector('meta[name="lm-csrf"]')?.content || '',
});
const views = {
  map: MapView,
  type: GridView,
  flat: GridView,
  skills: SkillsView,
  review: ReviewView,
  'flat-memory': ReviewView,
  timeline: TimelineView,
  entry: TimelineView,
  activity: ActivityView,
};

function renderProject(p) {
  return html`
    <a class="project-card" href=${href({ page: 'map', project: p.project })}>
      <i class=${p.live ? 'live-dot' : 'live-dot offline'}></i>
      <strong>${p.label || projectLabel(p.project)}</strong>
      <span>${p.live ? 'Live' : 'Not live'}</span>
    </a>
  `;
}

function renderProjects(projects, refresh) {
  return html`
    <div class="page-heading">
      <div class="projects-heading">
        <h1>Projects</h1>
        <button type="button" onClick=${refresh}>Refresh</button>
      </div>
      <p>Choose a local project. Opening this page does not start the worker.</p>
    </div>
    ${projects.length
      ? html`<div class="project-list">${projects.map(renderProject)}</div>`
      : html`<${Empty} title="No projects yet">
          Projects appear once an agent connects through the daemon.
        </${Empty}>`}
  `;
}

function App() {
  const [route, setRoute] = useState(() => parse(location.hash));
  const [projects, setProjects] = useState([]);
  const [error, setError] = useState(null);
  const [loaded, setLoaded] = useState(false);
  const [ids, setIds] = useState([]);
  const [verifiedRoute, setVerifiedRoute] = useState(null);
  const [lastProject, setLastProject] = useState(() => {
    try {
      return localStorage.getItem('lm-last-project');
    } catch {
      return null;
    }
  });
  const [theme, setTheme] = useState(() => {
    try {
      return localStorage.getItem('lm-theme') || 'auto';
    } catch {
      return 'auto';
    }
  });
  const last = useRef(null);
  const projectsRef = useRef(projects);
  const firstRoute = useRef(true);
  projectsRef.current = projects;
  const registry = useMemo(() => createProjectRegistry({
    load: () => api.api('/api/projects'),
    onUpdate: update => {
      if (!update.error) {
        setProjects(update.projects);
      }
      setError(update.error);
      setLoaded(true);
    },
  }), []);

  function subscribeRoute() {
    function update() {
      const next = parse(location.hash);
      if (next.project && next.project !== '@user' &&
          projectsRef.current.some(p => p.project === next.project)) {
        rememberSelection(next.project);
      }
      setRoute(next);
    }
    window.addEventListener('hashchange', update);
    return () => window.removeEventListener('hashchange', update);
  }

  function applyTheme() {
    if (theme === 'auto') {
      delete document.documentElement.dataset.theme;
    } else {
      document.documentElement.dataset.theme = theme;
    }
    try {
      localStorage.setItem('lm-theme', theme);
    } catch {}
  }

  function refresh() {
    return registry.refresh();
  }

  function loadProjects() {
    refresh();
  }

  function refreshOnVisibility() {
    return listenForVisible(document, refresh);
  }

  // Every navigation after the first rechecks the registry, so live dots and worker state never go stale.
  function refreshProjectsRoute() {
    if (firstRoute.current) {
      firstRoute.current = false;
    } else if (route.page !== 'memory') {
      refresh();
    }
  }
  if (route.page !== 'memory' && route.project) {
    last.current = route;
  }
  const base = route.page === 'memory'
    ? (last.current?.project === route.project ? last.current : { page: 'map', project: route.project })
    : route;
  const View = views[base.page] || MapView;
  const neighbors = useMemo(() => ({ ids, setIds }), [ids]);
  const entry = projects.find(p => p.project === route.project);

  function verifyMissingProject() {
    if (entry) {
      setVerifiedRoute(null);
      return;
    }
    if (!loaded || !route.project || error) {
      return;
    }
    let active = true;
    async function verify() {
      const result = await refresh();
      if (active && !result.error) {
        setVerifiedRoute(route);
      }
    }
    verify();
    return function cancelVerification() {
      active = false;
    };
  }

  function rememberSelection(project) {
    setLastProject(project);
    try {
      localStorage.setItem('lm-last-project', project);
    } catch {}
  }

  function rememberProject() {
    if (!entry || route.project === '@user') {
      return;
    }
    rememberSelection(route.project);
  }

  useEffect(subscribeRoute, []);
  useEffect(applyTheme, [theme]);
  useEffect(loadProjects, []);
  useEffect(refreshOnVisibility, []);
  useEffect(refreshProjectsRoute, [route]);
  useEffect(verifyMissingProject, [route, loaded, Boolean(entry), Boolean(error)]);
  useEffect(rememberProject, [route.project, entry?.project]);

  function closeMemory() {
    location.hash = href(base);
  }

  function openMemory(node) {
    location.hash = href({ page: 'memory', project: route.project, id: node });
  }

  function renderContent() {
    if (!loaded) {
      return html`<${Loading}/>`;
    }
    if (error) {
      return html`<${ErrorNote} error=${error}/>`;
    }
    if (!route.project) {
      return renderProjects(projects, refresh);
    }
    if (!entry) {
      if (verifiedRoute !== route) {
        return html`<${Loading}/>`;
      }
      return html`<${Empty} title="Unknown project">This project is not in the registry.</${Empty}>`;
    }
    if (!entry.live) {
      return html`<${NeedsStart} project=${route.project} onStarted=${refresh}/>`;
    }
    // Keyed by project: switching projects remounts the view and the modal, so no draft, selection,
    // dialog or in-flight paging from one project can act on another.
    return html`
      <${View} key=${base.project} project=${base.project} route=${base}/>
      ${route.page === 'memory' && html`
        <${MemoryModal} key=${route.project} project=${route.project} route=${route} node=${route.id}
          onClose=${closeMemory} onOpen=${openMemory}/>
      `}
    `;
  }

  return html`
    <${ApiContext.Provider} value=${api}>
      <${NeighborsContext.Provider} value=${neighbors}>
        <${Shell} project=${route.project} route=${base} projects=${projects} lastProject=${lastProject}
          theme=${theme} onTheme=${setTheme} onProjectsOpen=${refresh}>
          ${renderContent()}
        </${Shell}>
      </${NeighborsContext.Provider}>
    </${ApiContext.Provider}>
  `;
}

render(html`<${App}/>`, document.getElementById('app'));

import { html, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { href } from '../lib/router.mjs';
import { projectLabel } from '../lib/format.mjs';
import { projectTarget } from '../lib/projects.mjs';

// Renders navigation for project, route, projects, lastProject, theme, callbacks and children.
export function Shell({ project, route, projects, lastProject, theme, onTheme, onProjectsOpen, children }) {
  const [search, setSearch] = useState('');
  const selected = projects.find(p => p.project === project);
  const target = selected && project !== '@user' ? project : projectTarget(projects, lastProject);
  const nav = [
    ['map', 'Map'],
    ['skills', 'Skill drafts'],
    ['review', 'Review'],
    ['timeline', 'Timeline'],
    ['activity', 'Activity & diagnostics'],
  ];

  function updateSearch(event) {
    setSearch(event.currentTarget.value);
  }

  function changeTheme(event) {
    onTheme(event.currentTarget.value);
  }

  function toggleSwitcher(event) {
    if (event.currentTarget.open) {
      onProjectsOpen();
    }
  }

  function renderProject(p) {
    return html`
      <a href=${href({ page: 'map', project: p.project })}>
        <i class=${p.live ? 'live-dot' : 'live-dot offline'}></i>
        ${p.label || projectLabel(p.project)}
        <small>${p.live ? 'Live' : 'Not live'}</small>
      </a>
    `;
  }

  function renderNav([page, label]) {
    const current = route.page === page ||
      (page === 'map' && ['type', 'flat', 'memory'].includes(route.page));
    return html`
      <a href=${href({ page, project })} aria-current=${current ? 'page' : undefined}>${label}</a>
    `;
  }

  const matching = projects.filter(p => (p.label || p.project).toLowerCase().includes(search.toLowerCase()));
  return html`
    <header class="topbar">
      <a class="brand" href="#/"><span>λ</span> lambda memory</a>
      <details class="switcher" onToggle=${toggleSwitcher}>
        <summary>
          ${selected?.label || projectLabel(project) || 'Projects'} <span aria-hidden="true">⌄</span>
        </summary>
        <div class="switcher-menu">
          <input type="search" placeholder="Search projects" aria-label="Search projects"
            value=${search} onInput=${updateSearch}/>
          ${matching.map(renderProject)}
        </div>
      </details>
      <div class="segments" aria-label="Store">
        <a aria-current=${project && project !== '@user' ? 'page' : undefined}
          href=${target ? href({ page: 'map', project: target }) : '#/'}>Project</a>
        <a aria-current=${project === '@user' ? 'page' : undefined}
          href=${href({ page: 'map', project: '@user' })}>You</a>
      </div>
      <label class="theme">
        ${'Theme '}
        <select value=${theme} onChange=${changeTheme}>
          <option value="auto">Auto</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </label>
    </header>
    ${project && html`<nav class="nav" aria-label="Project navigation">${nav.map(renderNav)}</nav>`}
    <main>${children}</main>
  `;
}

// Renders a dismissible message and calls onClose when dismissed.
export function Toast({ message, onClose }) {
  return message && html`
    <div class="toast" role="status">
      ${message}<button onClick=${onClose}>Dismiss</button>
    </div>
  `;
}

import { html, useContext, useEffect, useLayoutEffect, useReducer, useRef, useState }
  from '../vendor/htm-preact-3.1.1.mjs';
import { Badge } from '../components/badge.mjs';
import { ActionDialog, Modal } from '../components/dialog.mjs';
import { ApiContext } from '../components/nav.mjs';
import { Empty, ErrorNote, Loading, NeedsStart } from '../components/states.mjs';
import { NOT_CONFIRMED, errorText } from '../lib/api.mjs';
import { href } from '../lib/router.mjs';
import { approveRequest, initialSkillReview, skillReview } from '../lib/skill-state.mjs';

const order = ['drafted', 'approved', 'installed', 'stale', 'rejected'];
const meanings = {
  drafted: 'Ready for review; not installed.',
  approved: 'Approved for installation; not yet installed.',
  installed: 'Installed on this device.',
  stale: 'No longer current; an earlier version may still be installed.',
  rejected: 'Rejected; this draft will not be installed.',
};

function PathList({ paths, explain = false }) {
  return html`
    <ul class="skill-paths">
      ${paths.map(item => html`
        <li>
          <div class="skill-path-heading"><strong>${item.action}</strong><code>${item.path}</code></div>
          ${explain && item.action === 'replace' && html`
            <p>Replaces Lambda's earlier file; a backup is kept next to it.</p>
          `}
          ${explain && item.action === 'blocked' && html`<p>Blocked: ${item.reason}</p>`}
        </li>
      `)}
    </ul>
  `;
}

function ResultPaths({ label, paths }) {
  if (!paths?.length) {
    return null;
  }
  return html`
    <div class="skill-result-paths">
      <strong>${label}</strong>
      <ul>${paths.map(path => html`<li><code>${path}</code></li>`)}</ul>
    </div>
  `;
}

export function SkillsView({ project, route }) {
  return html`<${SkillsProjectView} key=${project} project=${project} route=${route}/>`;
}

function SkillsProjectView({ project }) {
  const api = useContext(ApiContext);
  const activeProject = useRef(project);
  const [review, dispatch] = useReducer(skillReview, project, initialSkillReview);
  const { selected, preview, confirmation } = review;
  const [candidates, setCandidates] = useState(null);
  const [error, setError] = useState(null);
  const [generation, setGeneration] = useState(0);
  const [previewError, setPreviewError] = useState(null);
  const [previewGeneration, setPreviewGeneration] = useState(0);
  const [notice, setNotice] = useState(null);

  function trackProject() {
    activeProject.current = project;
    return () => { activeProject.current = null; };
  }

  useLayoutEffect(trackProject, [project]);

  function loadCandidates() {
    let alive = true;
    setCandidates(null);
    setError(null);
    api.api('/api/skill/list', { project }).then(result => {
      if (alive && activeProject.current === project) {
        setCandidates(result.candidates);
      }
    }).catch(err => {
      if (alive && activeProject.current === project) {
        setError(err);
      }
    });
    return () => { alive = false; };
  }

  function loadPreview() {
    if (!selected) {
      return;
    }
    let alive = true;
    const { project: selectedProject, candidate } = selected;
    setPreviewError(null);
    api.api('/api/skill/preview', { project: selectedProject, candidate }).then(result => {
      if (alive && activeProject.current === selectedProject) {
        dispatch({ type: 'preview', project: selectedProject, candidate, preview: result });
      }
    }).catch(err => {
      if (alive && activeProject.current === selectedProject) {
        setPreviewError(err);
      }
    });
    return () => { alive = false; };
  }

  useEffect(loadCandidates, [project, generation]);
  useEffect(loadPreview, [project, selected, previewGeneration]);

  function openCandidate(candidate) {
    setNotice(null);
    setPreviewError(null);
    dispatch({ type: 'select', project, candidate });
  }

  function closePreview() {
    dispatch({ type: 'clear', project });
    setNotice(null);
  }

  function updateStatus(candidate, status) {
    setCandidates(current => current.map(row => row.candidate_id === candidate ? { ...row, status } : row));
  }

  async function approve() {
    const view = confirmation.preview;
    try {
      const result = await api.api('/api/skill/approve', approveRequest(view));
      return { ...result, kind: 'installed', project: view.project, candidate: view.candidate };
    } catch (err) {
      if (err.status === 409 && err.kind !== 'needs_start') {
        return { kind: 'changed', project: view.project, candidate: view.candidate };
      }
      if (err.status === 502) {
        return { ...err.body, kind: 'unconfirmed', project: view.project, candidate: view.candidate };
      }
      throw err;
    }
  }

  function approved(result) {
    if (activeProject.current !== result.project) {
      return;
    }
    setNotice(result);
    if (result.kind === 'changed') {
      dispatch({ type: 'clear-preview', project: result.project });
      setPreviewGeneration(value => value + 1);
    } else if (result.kind === 'installed') {
      updateStatus(result.candidate, 'installed');
      dispatch({ type: 'status', project: result.project, candidate: result.candidate, status: 'installed' });
    }
  }

  async function reject() {
    const view = confirmation.preview;
    const result = await api.api('/api/skill/reject', {
      project: view.project,
      candidate: view.candidate,
      revision: view.revision_id,
    });
    if (result.isError) {
      throw new Error(errorText(result.body));
    }
    return { project: view.project, candidate: view.candidate };
  }

  function rejected(result) {
    if (activeProject.current !== result.project) {
      return;
    }
    updateStatus(result.candidate, 'rejected');
    dispatch({ type: 'status', project: result.project, candidate: result.candidate, status: 'rejected' });
    setNotice({ kind: 'rejected' });
  }

  if (error?.kind === 'needs_start') {
    return html`<${NeedsStart} project=${project} onStarted=${() => setGeneration(generation + 1)}/>`;
  }
  if (error) {
    return html`<${ErrorNote} error=${error}/>`;
  }
  if (!candidates) {
    return html`<${Loading}/>`;
  }
  const sorted = [...candidates].sort((a, b) => {
    const rank = status => order.indexOf(status) < 0 ? order.length : order.indexOf(status);
    return rank(a.status) - rank(b.status);
  });
  const canAct = preview && ['drafted', 'approved'].includes(preview.status) &&
    notice?.kind !== 'unconfirmed';
  const blocked = preview?.paths.some(item => item.action === 'blocked');
  return html`
    <div class="page-heading">
      <h1>Skill drafts</h1>
      <p>Review lessons proposed as skills before installing them on this device.</p>
    </div>
    ${sorted.length === 0 ? html`
      <${Empty} title="No skill drafts">
        Skill candidates will appear here when lessons produce drafts.
      </${Empty}>
    ` : html`
      <div class="skill-grid">
        ${sorted.map(row => html`
          <article class="skill-card">
            <div class="skill-card-top">
              <${Badge}>${row.scope} scope</${Badge}>
              <${Badge} title=${meanings[row.status]}>${row.status}</${Badge}>
            </div>
            <button class="skill-card-open" onClick=${() => openCandidate(row.candidate_id)}>
              ${row.slug}
            </button>
            <p class="skill-meaning">${meanings[row.status] || row.status}</p>
            <a class="skill-lesson" href=${href({ page: 'memory', project, id: row.memory_id })}>
              From lesson: ${row.memory_id}
            </a>
          </article>
        `)}
      </div>
    `}
    <${Modal} open=${Boolean(selected)} title=${preview?.slug || 'Skill draft'}
      onClose=${closePreview} wide=${true} footer=${preview && html`
        <div class="skill-actions">
          ${canAct && html`
            <button class="primary skill-approve" disabled=${blocked}
              onClick=${() => dispatch({ type: 'confirm', project, kind: 'approve' })}>
              Approve and install…
            </button>
            <button class="skill-reject"
              onClick=${() => dispatch({ type: 'confirm', project, kind: 'reject' })}>
              Reject draft
            </button>
          `}
          <button onClick=${closePreview}>Close</button>
        </div>
      `}>
      ${previewError?.kind === 'needs_start' ? html`
        <${NeedsStart} project=${project} onStarted=${() => setPreviewGeneration(value => value + 1)}/>
      ` : previewError ? html`<${ErrorNote} error=${previewError}/>` : !preview && html`<${Loading}/>`}
      ${notice?.kind === 'changed' && html`
        <p class="skill-notice" role="alert">The draft or the files changed; review again.</p>
      `}
      ${notice?.kind === 'installed' && html`
        <section class="skill-notice" role="status">
          <strong>Skill installed</strong>
          <${ResultPaths} label="Installed paths" paths=${notice.installed}/>
          <${ResultPaths} label="Backup paths" paths=${notice.backups}/>
        </section>
      `}
      ${notice?.kind === 'unconfirmed' && html`
        <section class="skill-notice" role="alert">
          <strong>${NOT_CONFIRMED}</strong>
          <${ResultPaths} label="Written paths" paths=${notice.written}/>
          <${ResultPaths} label="Backup paths" paths=${notice.backups}/>
        </section>
      `}
      ${notice?.kind === 'rejected' && html`<p class="skill-notice" role="status">Draft rejected.</p>`}
      ${preview && html`
        <div class="skill-preview-meta">
          <${Badge}>${preview.scope} scope</${Badge}>
          <${Badge} title=${meanings[preview.status]}>${preview.status}</${Badge}>
          <span>${meanings[preview.status]}</span>
        </div>
        <p><a class="skill-lesson" href=${href({ page: 'memory', project, id: preview.memory })}>
          From lesson: ${preview.memory}
        </a></p>
        <h3>Draft</h3>
        <pre class="skill-draft">${preview.draft}</pre>
        <h3>Install paths</h3>
        <${PathList} paths=${preview.paths} explain=${true}/>
        ${blocked && html`<p class="error">Resolve blocked paths before installing.</p>`}
      `}
    </${Modal}>
    <${ActionDialog} open=${confirmation?.kind === 'approve'} title="Approve and install"
      body=${confirmation?.kind === 'approve' && html`
        <p>This writes these files on this device.</p>
        <${PathList} paths=${confirmation.preview.paths} explain=${true}/>
      `}
      confirmLabel="Approve and install" run=${approve} onDone=${approved}
      onClose=${() => dispatch({ type: 'close-confirmation', project })}/>
    <${ActionDialog} open=${confirmation?.kind === 'reject'} title="Reject draft"
      body=${html`<p>Reject this draft? It will not be installed.</p>`}
      confirmLabel="Reject draft" danger=${true} run=${reject} onDone=${rejected}
      onClose=${() => dispatch({ type: 'close-confirmation', project })}/>
  `;
}

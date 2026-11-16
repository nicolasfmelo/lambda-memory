import { html, useContext, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { ApiContext } from './nav.mjs';
import { errorText } from '../lib/api.mjs';

// Renders a loading status; takes no props.
export function Loading() {
  return html`<p role="status" class="muted">Loading…</p>`;
}

// Renders an empty state with a title and children.
export function Empty({ title, children }) {
  return html`
    <section class="empty">
      <h2>${title}</h2>
      <p>${children}</p>
    </section>
  `;
}

// Renders an error message from the error prop.
export function ErrorNote({ error }) {
  return html`<p class="error" role="alert">${errorText(error)}</p>`;
}

// Renders a worker-start prompt for project and calls onStarted after success.
export function NeedsStart({ project, onStarted }) {
  const api = useContext(ApiContext);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);

  async function start() {
    setBusy(true);
    setError(null);
    try {
      await api.api('/api/start', { project });
      onStarted?.();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const billingNote = 'Starting the worker may process pending sources. Pending captures can be ' +
    'consolidated while the worker runs, which is billable in openrouter mode. ' +
    'The worker stops after the UI lease expires.';
  return html`
    <section class="empty start">
      <h2>Worker not running</h2>
      <p>Opening this page does not start the worker.</p>
      <p>${billingNote}</p>
      <button class="primary" disabled=${busy} onClick=${start}>
        ${busy ? 'Starting…' : 'Start worker'}
      </button>
      ${error && html`<${ErrorNote} error=${error}/>`}
    </section>
  `;
}

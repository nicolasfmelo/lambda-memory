import { html, useEffect, useRef, useState } from '../vendor/htm-preact-3.1.1.mjs';
import { errorText } from '../lib/api.mjs';

// Renders a native dialog with open, title, onClose, busy, wide, children and footer props.
export function Modal({ open, title, onClose, busy = false, wide = false, children, footer }) {
  const ref = useRef(null);
  const opener = useRef(null);

  function syncDialog() {
    const dialog = ref.current;
    if (!dialog) {
      return;
    }
    if (open && !dialog.open) {
      opener.current = document.activeElement;
      dialog.showModal();
    } else if (!open && dialog.open) {
      dialog.close();
      opener.current?.focus();
    }
    return function cleanupDialog() {
      if (dialog.open) {
        dialog.close();
      }
      opener.current?.focus();
    };
  }

  function cancel(event) {
    event.preventDefault();
    if (!busy) {
      onClose?.();
    }
  }

  useEffect(syncDialog, [open]);
  return html`
    <dialog ref=${ref} class=${wide ? 'modal wide' : 'modal'} aria-label=${title} onCancel=${cancel}>
      <header class="modal-head">
        <h2>${title}</h2>
        <button aria-label="Close" disabled=${busy} onClick=${onClose}>×</button>
      </header>
      <div class="modal-body">${children}</div>
      ${footer && html`<footer class="modal-footer">${footer}</footer>`}
    </dialog>
  `;
}

// Renders a confirmation: open, title, body, confirmLabel, danger, typed, run, onDone and onClose.
export function ActionDialog({ open, title, body, confirmLabel, danger, typed, run, onDone, onClose }) {
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);

  function resetOnOpen() {
    if (open) {
      setValue('');
      setError(null);
    }
  }

  async function submit(event) {
    event.preventDefault();
    if (busy || (typed !== undefined && value !== typed)) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await run();
      onDone?.(result);
      onClose?.();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  function updateValue(event) {
    setValue(event.currentTarget.value);
  }

  useEffect(resetOnOpen, [open]);
  const errorMessage = error?.kind === 'needs_start'
    ? 'The worker for this project is not running. Start the worker to continue.'
    : error && errorText(error);
  return html`
    <${Modal} open=${open} title=${title} onClose=${onClose} busy=${busy}>
      <form onSubmit=${submit}>
        ${body}
        ${typed !== undefined && html`
          <label>
            Type <code>${typed}</code> to confirm
            <input value=${value} onInput=${updateValue} autocomplete="off" />
          </label>
        `}
        ${error && html`<p class="error" role="alert">${errorMessage}</p>`}
        <footer class="modal-footer action-footer">
          <button type="button" disabled=${busy} onClick=${onClose}>Cancel</button>
          <button type="submit" class=${danger ? 'danger' : 'primary'}
            disabled=${busy || (typed !== undefined && value !== typed)}>
            ${busy ? 'Working…' : confirmLabel}
          </button>
        </footer>
      </form>
    </${Modal}>
  `;
}

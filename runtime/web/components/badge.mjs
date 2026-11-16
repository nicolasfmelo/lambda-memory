import { html } from '../vendor/htm-preact-3.1.1.mjs';

const meanings = {
  observed: 'Recorded from captured work. Provisional evidence, not verified truth.',
  concluded: 'Concluded by reflection from other memories; less certain than observed evidence.',
};

// Renders a badge with children, an optional title and semantic tone.
export function Badge({ children, title, tone }) {
  return html`<span class=${tone ? `badge badge-${tone}` : 'badge'} title=${title}>${children}</span>`;
}

// Renders a basis badge from basis and optional confidence.
export function BasisBadge({ basis, confidence }) {
  return html`
    <${Badge} title=${meanings[basis]} tone=${basis}>
      ${basis}${basis === 'concluded' && confidence ? ` · ${confidence}` : ''}
    </${Badge}>
  `;
}

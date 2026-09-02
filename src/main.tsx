/**
 * Entry point.
 *
 * The theme is applied to the document root *before* React mounts, so the first painted frame is
 * already the right colour. Doing it inside a component would show one frame of the wrong theme —
 * a flash that is small but unmistakable on a dark-themed application.
 */

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from '@/app/App';
import { DEFAULT_SETTINGS, applyPresentation } from '@/stores/settings';
import '@/styles/index.css';

applyPresentation(DEFAULT_SETTINGS);

const container = document.getElementById('root');
if (!container) {
  throw new Error('root element is missing from index.html');
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

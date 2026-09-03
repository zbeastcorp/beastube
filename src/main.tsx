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
import { ErrorBoundary } from '@/components/common/ErrorBoundary';
import { DEFAULT_SETTINGS, applyPresentation } from '@/stores/settings';
import '@/styles/index.css';

applyPresentation(DEFAULT_SETTINGS);

const container = document.getElementById('root');
if (!container) {
  throw new Error('root element is missing from index.html');
}

createRoot(container).render(
  <StrictMode>
    {/*
      The outermost boundary, outside every provider on purpose.

      What it catches may be the theme, the router, the translation catalogue or a store, so it can
      depend on none of them — hence plain text and inline styles rather than the design system.
      A last resort that needed the thing it is reporting on would fail for the same reason.
    */}
    <ErrorBoundary
      fallback={(error, reset) => (
        <div
          style={{
            padding: '2rem',
            font: '14px system-ui',
            color: '#fff',
            background: '#0f0f0f',
            minHeight: '100vh',
          }}
        >
          <h1 style={{ fontSize: '1rem', margin: '0 0 0.5rem' }}>
            BEASTUBE could not start this screen
          </h1>
          <p style={{ opacity: 0.7, margin: '0 0 1rem', maxWidth: '40rem' }}>
            Your library, playlists and bookmarks are safe — they are stored on this device and this
            failure did not touch them.
          </p>
          <pre
            style={{ opacity: 0.5, fontSize: '12px', whiteSpace: 'pre-wrap', margin: '0 0 1rem' }}
          >
            {error.message}
          </pre>
          <button
            type="button"
            onClick={reset}
            style={{ padding: '0.5rem 1rem', borderRadius: '999px', border: 0, cursor: 'pointer' }}
          >
            Try again
          </button>
        </div>
      )}
    >
      <App />
    </ErrorBoundary>
  </StrictMode>,
);

/**
 * The boundary that stops one bad render blanking the window.
 *
 * Worth testing rather than eyeballing, because the failure it prevents is invisible until it
 * happens: without a boundary React unmounts the whole tree, and the symptom is an empty window
 * with nothing in it to suggest what went wrong or what to do.
 */

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ErrorBoundary } from '@/components/common/ErrorBoundary';

/** Throws on render when asked to. */
function Fragile({ explode }: { explode: boolean }): React.ReactNode {
  if (explode) throw new Error('the card could not be rendered');
  return <p>the real screen</p>;
}

function fallback(error: Error, reset: () => void): React.ReactNode {
  return (
    <div>
      <p>something went wrong: {error.message}</p>
      <button type="button" onClick={reset}>
        try again
      </button>
    </div>
  );
}

beforeEach(() => {
  // React writes the caught error to the console itself, and so does `componentDidCatch`. Both are
  // correct in production and pure noise here, so they are silenced for the duration rather than
  // removed from the component.
  vi.spyOn(console, 'error').mockImplementation(() => undefined);
});

afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
});

describe('ErrorBoundary', () => {
  it('renders its children when nothing throws', () => {
    render(
      <ErrorBoundary fallback={fallback}>
        <Fragile explode={false} />
      </ErrorBoundary>,
    );
    expect(screen.getByText('the real screen')).toBeTruthy();
  });

  it('shows the fallback instead of unmounting the tree', () => {
    render(
      <ErrorBoundary fallback={fallback}>
        <Fragile explode />
      </ErrorBoundary>,
    );
    expect(screen.getByText(/the card could not be rendered/u)).toBeTruthy();
  });

  it('leaves everything outside it untouched', () => {
    // The whole point of scoping the boundary to the routed view: the shell around it keeps
    // working, so a broken screen is one you can navigate away from rather than a trap.
    render(
      <div>
        <nav>sidebar</nav>
        <ErrorBoundary fallback={fallback}>
          <Fragile explode />
        </ErrorBoundary>
      </div>,
    );
    expect(screen.getByText('sidebar')).toBeTruthy();
    expect(screen.getByText(/something went wrong/u)).toBeTruthy();
  });

  it('recovers when the fallback asks it to', () => {
    function Harness(): React.ReactNode {
      return (
        <ErrorBoundary fallback={fallback}>
          <Fragile explode={false} />
        </ErrorBoundary>
      );
    }

    const view = render(
      <ErrorBoundary fallback={fallback}>
        <Fragile explode />
      </ErrorBoundary>,
    );
    fireEvent.click(screen.getByText('try again'));

    // Re-rendered with children that no longer throw, the boundary is transparent again.
    view.rerender(<Harness />);
    expect(screen.getByText('the real screen')).toBeTruthy();
  });

  it('clears itself when the reset key changes', () => {
    // This is what makes navigating away the recovery. Without it a boundary that caught once would
    // keep showing its fallback over the *next* screen, which looks like the failure spreading.
    const view = render(
      <ErrorBoundary resetKey="home" fallback={fallback}>
        <Fragile explode />
      </ErrorBoundary>,
    );
    expect(screen.getByText(/something went wrong/u)).toBeTruthy();

    view.rerender(
      <ErrorBoundary resetKey="shorts" fallback={fallback}>
        <Fragile explode={false} />
      </ErrorBoundary>,
    );
    expect(screen.getByText('the real screen')).toBeTruthy();
  });

  it('stays in the fallback while the reset key is unchanged', () => {
    // A re-render for any other reason must not clear the error, or the boundary would flap between
    // the fallback and a screen that throws again immediately.
    const view = render(
      <ErrorBoundary resetKey="home" fallback={fallback}>
        <Fragile explode />
      </ErrorBoundary>,
    );
    view.rerender(
      <ErrorBoundary resetKey="home" fallback={fallback}>
        <Fragile explode />
      </ErrorBoundary>,
    );
    expect(screen.getByText(/something went wrong/u)).toBeTruthy();
  });
});

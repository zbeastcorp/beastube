/**
 * Stops a render error taking the whole application with it.
 *
 * React unmounts the entire tree when a render throws and nothing catches it. Without a boundary
 * anywhere — which was the state of this application — a single bad field in one card blanked the
 * window permanently, with no way back but restarting. That is the worst failure mode a desktop app
 * has, because the user cannot even reach the screen that still works.
 *
 * ## Two layers, deliberately
 *
 * The shell mounts this twice, and the nesting is the point:
 *
 * 1. **Around the routed view**, inside the providers. A view that throws is replaced by a message
 *    while the sidebar, the top bar and navigation keep working — so a broken Home is something you
 *    walk away from rather than something that traps you. It resets when the route changes, so
 *    navigating away *is* the recovery.
 * 2. **Around everything**, outside the providers. This one can rely on nothing — not translations,
 *    not stores, not the theme — because the thing it exists to catch might be any of them. Its
 *    fallback is deliberately plain text and inline styles: a last resort that cannot itself fail
 *    for the same reason as whatever it is reporting.
 *
 * ## Why a class
 *
 * `getDerivedStateFromError` has no hook equivalent. This is the one part of React that still
 * requires a class, and wrapping it in something fashionable would only hide that.
 */

import { Component, type ErrorInfo, type ReactNode } from 'react';

export interface ErrorBoundaryProps {
  children: ReactNode;
  /**
   * Changing this clears the error.
   *
   * The route name is what the shell passes: navigating away is the natural recovery, and without
   * it a boundary that caught once would keep showing its fallback over the *next* screen too.
   */
  resetKey?: string | number;
  /** Rendered instead of the children once something has thrown. */
  fallback: (error: Error, reset: () => void) => ReactNode;
}

interface ErrorBoundaryState {
  error: Error | null;
  /** The `resetKey` the current error belongs to, so a change to it clears the error. */
  seenKey: string | number | undefined;
}

/** Catches render errors below it and shows `fallback` instead of unmounting the tree. */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  override state: ErrorBoundaryState = { error: null, seenKey: undefined };

  static getDerivedStateFromError(error: Error): Partial<ErrorBoundaryState> {
    return { error };
  }

  static getDerivedStateFromProps(
    props: ErrorBoundaryProps,
    state: ErrorBoundaryState,
  ): Partial<ErrorBoundaryState> | null {
    // Cleared by a change of key rather than by an effect: this runs during the same render that
    // brings the new key in, so the recovered screen paints once instead of painting the fallback
    // and then replacing it.
    if (state.error !== null && state.seenKey !== undefined && state.seenKey !== props.resetKey) {
      return { error: null, seenKey: props.resetKey };
    }
    if (state.error === null) {
      return { seenKey: props.resetKey };
    }
    return null;
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    // The component stack is the only part of this that is hard to reconstruct afterwards, and it
    // is what turns "something threw" into a place to look.
    console.error('render failed', error, info.componentStack);
  }

  private readonly reset = (): void => {
    this.setState({ error: null });
  };

  override render(): ReactNode {
    const { error } = this.state;
    if (error !== null) {
      return this.props.fallback(error, this.reset);
    }
    return this.props.children;
  }
}

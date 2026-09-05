/**
 * Router runtime.
 *
 * Backed by the browser history API through the location hash, so back/forward, the mouse's side
 * buttons and the desktop shell's navigation all work without extra wiring.
 *
 * Navigation is synchronous and never suspends. A route change swaps the view immediately and the
 * view fetches its own data, showing cached content or a skeleton (§87) — the alternative, waiting
 * for data before committing the navigation, is the most common reason desktop applications built
 * on web technology feel sluggish.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type AnchorHTMLAttributes,
  type ReactNode,
} from 'react';

import { hashToRoute, isSameRoute, routeToHash, type Route } from './routes';

interface RouterValue {
  route: Route;
  navigate: (to: Route, options?: { replace?: boolean }) => void;
  back: () => void;
  forward: () => void;
  canGoBack: boolean;
}

const RouterContext = createContext<RouterValue | null>(null);

function currentHash(): string {
  return typeof window === 'undefined' ? '#/' : window.location.hash || '#/';
}

function subscribeToHash(onChange: () => void): () => void {
  window.addEventListener('hashchange', onChange);
  window.addEventListener('popstate', onChange);
  return () => {
    window.removeEventListener('hashchange', onChange);
    window.removeEventListener('popstate', onChange);
  };
}

/** Provides routing to the tree. */
export function RouterProvider({ children }: { children: ReactNode }): ReactNode {
  // useSyncExternalStore rather than state plus an effect: it keeps the rendered route consistent
  // with window.location across concurrent renders, which an effect-driven mirror does not.
  const hash = useSyncExternalStore(subscribeToHash, currentHash, () => '#/');
  const route = useMemo(() => hashToRoute(hash), [hash]);

  // The history API exposes a length but no "can go back" flag, so depth is tracked explicitly:
  // without it the back control would look enabled on the first screen and do nothing.
  //
  // The ref is the authority (event handlers mutate it synchronously); the state mirrors it purely
  // so render has something legal to read — reading `ref.current` during render is a correctness
  // hazard, because React is not told the value changed.
  const depth = useRef(0);
  const [canGoBack, setCanGoBack] = useState(false);

  const navigate = useCallback((to: Route, options?: { replace?: boolean }) => {
    const target = routeToHash(to);
    if (target === window.location.hash) return;

    const replace = options?.replace ?? isSameRoute(hashToRoute(window.location.hash), to);
    if (replace) {
      window.history.replaceState(null, '', target);
      // replaceState does not fire hashchange; dispatch so subscribers observe the change.
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    } else {
      depth.current += 1;
      setCanGoBack(true);
      window.location.hash = target;
    }
  }, []);

  const back = useCallback(() => {
    if (depth.current > 0) {
      depth.current -= 1;
      setCanGoBack(depth.current > 0);
      window.history.back();
    }
  }, []);

  const forward = useCallback(() => {
    // Mirrors `back`, which decrements the depth. Without this the counter only ever fell, so one
    // Back followed by one Forward left it at zero and `back` — guarded on `depth > 0` — refused
    // to move again for the rest of the session, taking the mouse's Back button with it.
    // Over-counting where there is no forward entry is harmless: `history.forward()` is then a
    // no-op and the next `back` simply decrements again.
    depth.current += 1;
    setCanGoBack(true);
    window.history.forward();
  }, []);

  // Mouse side buttons. Desktop users expect these to navigate; inside a webview they do nothing
  // unless wired up.
  useEffect(() => {
    const onMouseUp = (event: MouseEvent) => {
      if (event.button === 3) {
        event.preventDefault();
        back();
      } else if (event.button === 4) {
        event.preventDefault();
        forward();
      }
    };
    window.addEventListener('mouseup', onMouseUp);
    return () => {
      window.removeEventListener('mouseup', onMouseUp);
    };
  }, [back, forward]);

  const value = useMemo<RouterValue>(
    () => ({ route, navigate, back, forward, canGoBack }),
    [route, navigate, back, forward, canGoBack],
  );

  return <RouterContext value={value}>{children}</RouterContext>;
}

function useRouter(): RouterValue {
  const value = useContext(RouterContext);
  if (!value) throw new Error('Router hooks must be used within a RouterProvider');
  return value;
}

/** The current route. */
export function useRoute(): Route {
  return useRouter().route;
}

/** Navigates to a route. The returned function is stable across renders. */
export function useNavigate(): RouterValue['navigate'] {
  return useRouter().navigate;
}

/**
 * An anchor that navigates in-app.
 *
 * A real anchor with an href rather than a click-handling div, so middle-click, copy-link and
 * screen readers behave as users expect; the handler intercepts only a plain left click.
 */
export function Link({
  to,
  children,
  className,
  // Pulled out of `rest` rather than spread with it. The spread lands *after* this element's own
  // handler, so a caller's `onClick` used to replace navigation outright instead of running
  // alongside it — a link that silently stopped linking. The two are composed here: the caller's
  // handler runs first and can cancel the navigation with `preventDefault`, exactly as it could on
  // a plain anchor.
  onClick,
  ...rest
}: { to: Route; children: ReactNode; className?: string } & Omit<
  AnchorHTMLAttributes<HTMLAnchorElement>,
  'href'
>): ReactNode {
  const navigate = useNavigate();
  return (
    <a
      href={routeToHash(to)}
      className={className}
      {...rest}
      onClick={(event) => {
        onClick?.(event);
        // Modified clicks mean "open elsewhere"; hijacking them is a long-standing annoyance in
        // web-technology desktop apps.
        if (event.defaultPrevented || event.button !== 0) return;
        if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
        event.preventDefault();
        navigate(to);
      }}
    >
      {children}
    </a>
  );
}

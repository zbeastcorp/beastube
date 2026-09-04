/**
 * The application shell.
 *
 * Composes the providers, the chrome and the routed view. Two decisions here matter for perceived
 * speed (§86, §87):
 *
 * 1. **Settings load asynchronously, and the shell does not wait for them.** Defaults are already
 *    correct, so the window paints immediately and the loaded document swaps in. Blocking the first
 *    paint on IPC is the difference between an application that opens instantly and one that shows
 *    a blank frame first.
 * 2. **The route swaps synchronously.** Views fetch their own data and render cached content or a
 *    skeleton; navigation is never gated on a request.
 */

import { useEffect, useState, type ReactNode } from 'react';

import { ErrorBoundary } from '@/components/common/ErrorBoundary';
import { NavigationProgress } from '@/components/shell/NavigationProgress';
import { PlayerHost } from '@/components/video/PlayerHost';
import { OverlayHost } from '@/components/shell/OverlayHost';
import { ToastHost } from '@/components/shell/ToastHost';
import { preloadPlayerApi } from '@/components/video/YouTubePlayer';
import { Sidebar } from '@/components/shell/Sidebar';
import { TopBar } from '@/components/shell/TopBar';
import { detectLocale, resolveLocale } from '@/i18n';
import { useTranslation } from '@/i18n/context';
import { TranslationProvider } from '@/i18n/context';
import { loadCapabilities } from '@/services/capabilities';
import { preloadFeeds } from '@/services/feedCache';
import { subscribeToDownloads, useDownloadsStore } from '@/stores/downloads';
import { useFeedStore } from '@/stores/feed';
import { invoke, isTauriRuntime, listen } from '@/services/ipc';
import { applyPresentation, applyWebviewScheme, useSettingsStore } from '@/stores/settings';
import { useSessionStore } from '@/stores/session';
import { useUiStore } from '@/stores/ui';

import { RouterProvider, useRoute } from './router';
import { renderRoute } from './views';

/** Applies presentation settings to the document root whenever they change. */
function usePresentation(): void {
  const settings = useSettingsStore((state) => state.settings);

  useEffect(() => {
    applyPresentation(settings);
    // And the webview itself, so the embedded player's own settings panel is painted in the same
    // scheme. It is another origin's document; its colours answer to the webview preference and to
    // nothing we can style.
    applyWebviewScheme(settings);
  }, [settings]);

  // Follow the OS colour scheme while the theme is "system". Without this, changing the Windows
  // theme while the application is open leaves it on the stale one until restart.
  useEffect(() => {
    if (settings.appearance.theme !== 'system') return undefined;
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = () => {
      applyPresentation(settings);
      applyWebviewScheme(settings);
    };
    media.addEventListener('change', onChange);
    return () => {
      media.removeEventListener('change', onChange);
    };
  }, [settings]);
}

/** Subscribes to the native events the shell itself reacts to. */
function useShellEvents(): void {
  const setNetworkStatus = useSessionStore((state) => state.setNetworkStatus);
  const toast = useUiStore((state) => state.toast);
  const dismissToast = useUiStore((state) => state.dismissToast);

  /**
   * Connectivity, from the webview rather than from the native side.
   *
   * The native side declares a `network:changed` event and the frontend has always listened for
   * it, but nothing anywhere emits it — so `networkStatus` was permanently "online" whatever was
   * actually true, and the offline notice never appeared. The webview knows, and its `online` and
   * `offline` events are the same signal without a round trip.
   *
   * `navigator.onLine` is optimistic: it reports a working interface, not a working route to the
   * internet. That is fine for what this is used for. Going "online" only *triggers a refetch*,
   * which either succeeds or fails exactly as it would have anyway — the cost of believing it too
   * readily is one request, and the cost of not believing it is a screen stuck on an error for a
   * condition that has passed.
   */
  useEffect(() => {
    let offlineNotice: string | null = null;

    const goOffline = () => {
      setNetworkStatus('offline');
      offlineNotice ??= toast({
        messageKey: 'error.network.offline',
        tone: 'warning',
        durationMs: null,
      });
    };

    const goOnline = () => {
      setNetworkStatus('online');
      if (offlineNotice !== null) {
        dismissToast(offlineNotice);
        offlineNotice = null;
      }
      // A feed that failed while the connection was down would otherwise sit on its error until
      // someone thought to press Retry, for a condition the application already knows has passed.
      // The cache keeps whatever was on screen until the new batch lands.
      useFeedStore.getState().refresh();
    };

    // The window may already be offline when the shell mounts, in which case no event is coming.
    if (!navigator.onLine) goOffline();

    window.addEventListener('offline', goOffline);
    window.addEventListener('online', goOnline);

    return () => {
      window.removeEventListener('offline', goOffline);
      window.removeEventListener('online', goOnline);
    };
  }, [setNetworkStatus, toast, dismissToast]);

  useEffect(() => {
    const unlistenFilter = listen('filter:updated', (payload) => {
      if (payload.outcome === 'rolled_back') {
        toast({
          messageKey: 'error.filtering.rolled_back',
          tone: 'warning',
          durationMs: 6000,
        });
      }
    });

    return () => {
      unlistenFilter();
    };
  }, [toast]);

  /**
   * Downloads, subscribed once for the whole application.
   *
   * A download outlives the screen that started it, so the subscription cannot live on the button:
   * one that finishes while the user is on another video still has to announce itself, and a
   * button remounting must not re-announce a download it merely learned about. The session's
   * existing downloads are read once, so a reloaded shell shows what is still running.
   */
  useEffect(() => {
    const unsubscribe = subscribeToDownloads();
    const downloads = useDownloadsStore.getState();
    void downloads.hydrate();
    // What is installed decides whether the control exists at all, so it is read at startup
    // rather than when a button first renders.
    void downloads.refreshTools();
    return unsubscribe;
  }, []);
}

/** The chrome plus the routed view. */
function Shell(): ReactNode {
  const collapsed = useUiStore((state) => state.sidebarCollapsed);
  const route = useRoute();
  const [scroller, setScroller] = useState<HTMLElement | null>(null);

  usePresentation();
  useShellEvents();

  // What the route key used to do by remounting. A new screen starts at the top; without this it
  // would inherit wherever the previous one had been scrolled to.
  useEffect(() => {
    scroller?.scrollTo({ top: 0, behavior: 'auto' });
  }, [route.name, scroller]);

  return (
    <div className="bg-bg text-text flex h-full flex-col overflow-hidden">
      {/* Above the top bar in the stacking order and outside the layout flow, so raising it never
          moves anything — as on YouTube, where the bar overlays the masthead rather than shifting
          it down two pixels. */}
      <NavigationProgress />
      {/* At the root, so a dialog outlives whatever opened it — a menu that closes on click would
          otherwise take the dialog it just opened down with it. */}
      <OverlayHost />
      {/* Likewise at the root: a download that finishes after you have navigated away still has
          to say so, and a notice mounted inside a screen dies with that screen. */}
      <ToastHost />
      <TopBar />
      <div className="flex min-h-0 flex-1">
        <Sidebar collapsed={collapsed} />
        <main
          id="main-content"
          ref={setScroller}
          // `relative` so the player host below can position itself against this box, and no longer
          // keyed on the route. The key used to live here, which meant every navigation rebuilt the
          // whole subtree — including the player, and therefore its `<iframe>`. Scroll is reset
          // explicitly instead, which is all the key was really buying.
          className="scroll-region relative min-w-0 flex-1"
        >
          <div
            key={route.name}
            className="animate-route-in mx-auto max-w-[var(--layout-content-max)] px-6 pt-2 pb-16"
          >
            {/* Scoped to the view, so a screen that throws is something you can walk away from:
                the sidebar and the top bar are outside this and keep working. Reset on the route
                name, which makes navigating away the recovery. */}
            <ErrorBoundary
              resetKey={route.name}
              fallback={(_error, reset) => <RouteFailure onRetry={reset} />}
            >
              {renderRoute(route)}
            </ErrorBoundary>
          </div>

          {/* Inside the scroller so it moves with the content for free, outside the keyed subtree
              so navigation cannot destroy it. */}
          <PlayerHost scroller={scroller} />
        </main>
      </div>
    </div>
  );
}

/**
 * Shown in place of one screen that failed to render.
 *
 * Deliberately not the whole-window surface: everything around it still works, and saying so is
 * most of the value — the alternative is a blank frame that looks like the application died.
 */
function RouteFailure({ onRetry }: { onRetry: () => void }): ReactNode {
  const t = useTranslation();
  return (
    <div className="flex flex-col items-start gap-3 py-16">
      <h2 className="text-text text-base font-medium">{t.t('error.generic')}</h2>
      <p className="text-text-muted max-w-prose text-sm">{t.t('error.genericHint')}</p>
      <button
        type="button"
        onClick={onRetry}
        className="transition-surface bg-surface hover:bg-surface-hover text-text rounded-full px-4 py-2 text-sm font-medium"
      >
        {t.t('app.retry')}
      </button>
    </div>
  );
}

/** Wires the providers around the shell. */
function Providers({ children }: { children: ReactNode }): ReactNode {
  const language = useSettingsStore((state) => state.settings.appearance.language);
  const locale = language === null ? detectLocale() : resolveLocale(language);

  return (
    <TranslationProvider locale={locale}>
      <RouterProvider>{children}</RouterProvider>
    </TranslationProvider>
  );
}

/** Kept in step with the limits the two shorts surfaces ask for, so the preload warms their keys. */
const SHORTS_FEED_LIMIT = 40;
const HOME_SHORTS_LIMIT = 16;

/** Matches `RECOMMENDED_COUNT` in the views; the preload must ask for the same size to be reused. */
const RECOMMENDED_LIMIT = 36;

/** The application root. */
export function App(): ReactNode {
  const load = useSettingsStore((state) => state.load);
  const setSidebarCollapsed = useUiStore((state) => state.setSidebarCollapsed);

  // Fire-and-forget: the shell renders with defaults and updates when this resolves, so a slow or
  // failed load never delays the first paint.
  useEffect(() => {
    void load().then(() => {
      const { settings } = useSettingsStore.getState();
      setSidebarCollapsed(settings.appearance.sidebar_collapsed);
    });
  }, [load, setSidebarCollapsed]);

  // Warm the feeds before anything asks for them. The Shorts tab and Home's shelf both read the
  // same cached batch, so by the time either is opened the request has usually already landed —
  // which is the difference between arriving at a feed and arriving at a spinner.
  useEffect(() => {
    if (!isTauriRuntime()) return;
    preloadFeeds([SHORTS_FEED_LIMIT, HOME_SHORTS_LIMIT], RECOMMENDED_LIMIT);
    // Capabilities decide which controls exist at all, so they are wanted before the first screen
    // that gates on them rather than after it.
    void loadCapabilities().catch(() => {
      // Speculative; a view that needs the answer asks again through the same cache.
    });
    // And the player API, for the same reason. It is a cross-origin script that must be fetched,
    // parsed and run before the first player can exist, and paying for that on the first click
    // means the viewer waits for it with nothing on screen. Started here, it has almost always
    // resolved before a video is opened.
    preloadPlayerApi();
  }, []);

  // Tell the native side the first frame is on screen, so it can reveal the window. Two frames,
  // because one only guarantees the commit has been scheduled, not that it has been painted.
  //
  // Held until settings have resolved. The shell paints with defaults — an expanded sidebar, a
  // 100% interface scale — and settings can disagree with every one of them, so revealing the
  // window first meant watching the layout jump into place a moment after it appeared. The shell
  // reads settings from a local SQLite row, so this waits milliseconds; the watchdog in the native
  // side reveals the window anyway if it ever waits longer.
  const settingsLoaded = useSettingsStore((state) => state.loaded);
  useEffect(() => {
    if (!isTauriRuntime() || !settingsLoaded) return undefined;
    const frame = requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        void invoke('frontend_ready', undefined).catch(() => {
          // The watchdog in the shell shows the window regardless; a failure here is not fatal.
        });
      });
    });
    return () => {
      cancelAnimationFrame(frame);
    };
  }, [settingsLoaded]);

  return (
    <Providers>
      <Shell />
    </Providers>
  );
}

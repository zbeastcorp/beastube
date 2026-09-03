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

import { useEffect, type ReactNode } from 'react';

import { Sidebar } from '@/components/shell/Sidebar';
import { TopBar } from '@/components/shell/TopBar';
import { detectLocale, resolveLocale } from '@/i18n';
import { TranslationProvider } from '@/i18n/context';
import { preloadFeeds } from '@/services/feedCache';
import { invoke, isTauriRuntime, listen } from '@/services/ipc';
import { applyPresentation, useSettingsStore } from '@/stores/settings';
import { useSessionStore } from '@/stores/session';
import { useUiStore } from '@/stores/ui';

import { RouterProvider, useRoute } from './router';
import { renderRoute } from './views';

/** Applies presentation settings to the document root whenever they change. */
function usePresentation(): void {
  const settings = useSettingsStore((state) => state.settings);

  useEffect(() => {
    applyPresentation(settings);
  }, [settings]);

  // Follow the OS colour scheme while the theme is "system". Without this, changing the Windows
  // theme while the application is open leaves it on the stale one until restart.
  useEffect(() => {
    if (settings.appearance.theme !== 'system') return undefined;
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = () => {
      applyPresentation(settings);
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

  useEffect(() => {
    const unlistenNetwork = listen('network:changed', (payload) => {
      setNetworkStatus(payload.current);
      if (payload.current === 'offline') {
        toast({
          messageKey: 'error.network.offline',
          tone: 'warning',
          durationMs: null,
        });
      }
    });

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
      unlistenNetwork();
      unlistenFilter();
    };
  }, [setNetworkStatus, toast]);
}

/** The chrome plus the routed view. */
function Shell(): ReactNode {
  const collapsed = useUiStore((state) => state.sidebarCollapsed);
  const route = useRoute();

  usePresentation();
  useShellEvents();

  return (
    <div className="bg-bg text-text flex h-full flex-col overflow-hidden">
      <TopBar />
      <div className="flex min-h-0 flex-1">
        <Sidebar collapsed={collapsed} />
        <main
          id="main-content"
          className="scroll-region min-w-0 flex-1"
          // Keyed on the route so a new view starts at the top rather than inheriting the previous
          // view's scroll offset, and so its enter animation replays.
          key={route.name}
        >
          <div className="animate-fade-in mx-auto max-w-[var(--layout-content-max)] px-6 pt-2 pb-16">
            {renderRoute(route)}
          </div>
        </main>
      </div>
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
    preloadFeeds([SHORTS_FEED_LIMIT, HOME_SHORTS_LIMIT]);
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

/**
 * The settings screen.
 *
 * Every control writes straight into the settings store, which updates in memory immediately and
 * persists on a debounce — so dragging a slider is smooth and the native side is not asked to write
 * SQLite thirty times a second.
 *
 * The privacy panel is deliberately the most detailed. It is where the application's claims about
 * itself are made checkable: where data lives, how much of it there is, and a control to remove each
 * kind. A privacy promise that cannot be inspected is just a sentence (§100).
 */

import { useState, type ReactNode } from 'react';

import {
  DangerButton,
  ReadOnlyValue,
  SecondaryButton,
  Select,
  SettingRow,
  SettingsSection,
  Slider,
  Switch,
  useControlId,
} from '@/components/common/Controls';
import { useAsyncResource } from '@/hooks/useAsyncResource';
import { LOCALE_NAMES, SUPPORTED_LOCALES } from '@/i18n';
import { useTranslation } from '@/i18n/context';
import { clearFeedCache } from '@/services/feedCache';
import { invoke } from '@/services/ipc';
import { clearVideoCache } from '@/services/videoCache';
import { activePlaybackCapabilities } from '@/services/playback';
import { checkForUpdate, downloadAndInstallUpdate, relaunchApp } from '@/services/updates';
import { useDownloadsStore } from '@/stores/downloads';
import { useSessionStore } from '@/stores/session';
import { useSettingsStore } from '@/stores/settings';
import type { Update } from '@tauri-apps/plugin-updater';

import type { Density, FilteringMode, Quality, Theme } from '@/types/domain';
import { isTerminalDownload } from '@/types/domain';

/** Quality tiers offered as a ceiling. `auto` is not a ceiling, so it is excluded. */
const QUALITY_TIERS: Quality[] = ['360p', '480p', '720p', '1080p', '1440p', '2160p'];

function AppearancePanel(): ReactNode {
  const t = useTranslation();
  const appearance = useSettingsStore((state) => state.settings.appearance);
  const update = useSettingsStore((state) => state.update);

  const themeId = useControlId('theme');
  const densityId = useControlId('density');
  const languageId = useControlId('language');
  const scaleId = useControlId('scale');

  const themes: { value: Theme; label: string }[] = [
    { value: 'system', label: t.t('settings.appearance.themeSystem') },
    { value: 'dark', label: t.t('settings.appearance.themeDark') },
    { value: 'light', label: t.t('settings.appearance.themeLight') },
    { value: 'amoled', label: t.t('settings.appearance.themeAmoled') },
  ];

  return (
    <SettingsSection title={t.t('settings.appearance.title')}>
      <SettingRow label={t.t('settings.appearance.theme')} htmlFor={themeId}>
        <Select
          id={themeId}
          value={appearance.theme}
          options={themes}
          onChange={(theme) => {
            update({ appearance: { theme } });
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.appearance.density')} htmlFor={densityId}>
        <Select
          id={densityId}
          value={appearance.density}
          options={
            [
              { value: 'comfortable', label: t.t('settings.appearance.densityComfortable') },
              { value: 'compact', label: t.t('settings.appearance.densityCompact') },
            ] satisfies { value: Density; label: string }[]
          }
          onChange={(density) => {
            update({ appearance: { density } });
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.appearance.language')} htmlFor={languageId}>
        <Select
          id={languageId}
          value={appearance.language ?? 'system'}
          options={[
            { value: 'system', label: t.t('settings.appearance.languageSystem') },
            ...SUPPORTED_LOCALES.map((locale) => ({
              value: locale,
              label: LOCALE_NAMES[locale],
            })),
          ]}
          onChange={(language) => {
            update({ appearance: { language: language === 'system' ? null : language } });
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.appearance.uiScale')} htmlFor={scaleId}>
        <Slider
          id={scaleId}
          value={appearance.ui_scale}
          min={0.75}
          max={2}
          step={0.05}
          format={(value) => `${Math.round(value * 100)}%`}
          onChange={(ui_scale) => {
            update({ appearance: { ui_scale } });
          }}
        />
      </SettingRow>

      <SettingRow
        label={t.t('settings.appearance.ambientMode')}
        hint={t.t('settings.appearance.ambientModeHint')}
      >
        <Switch
          label={t.t('settings.appearance.ambientMode')}
          checked={appearance.ambient_mode}
          onChange={(ambient_mode) => {
            update({ appearance: { ambient_mode } });
          }}
        />
      </SettingRow>

      <SettingRow
        label={t.t('settings.appearance.reducedMotion')}
        hint={t.t('settings.appearance.reducedMotionSystem')}
      >
        <Switch
          label={t.t('settings.appearance.reducedMotion')}
          checked={appearance.reduced_motion === true}
          onChange={(enabled) => {
            // `null` means "follow the OS"; an explicit false is a deliberate override, so toggling
            // off returns to following the system rather than forcing motion on.
            update({ appearance: { reduced_motion: enabled ? true : null } });
          }}
        />
      </SettingRow>
    </SettingsSection>
  );
}

function PlaybackPanel(): ReactNode {
  const t = useTranslation();
  const playback = useSettingsStore((state) => state.settings.playback);
  const update = useSettingsStore((state) => state.update);

  const maxQualityId = useControlId('max-quality');
  const controlsId = useControlId('player-controls');
  const seekId = useControlId('seek');
  const speedId = useControlId('speed');

  const capabilities = activePlaybackCapabilities();

  return (
    <SettingsSection title={t.t('settings.playback.title')}>
      <SettingRow label={t.t('settings.playback.autoplayOnOpen')}>
        <Switch
          label={t.t('settings.playback.autoplayOnOpen')}
          checked={playback.autoplay_on_open}
          onChange={(autoplay_on_open) => {
            update({ playback: { autoplay_on_open } });
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.playback.autoplayNext')}>
        <Switch
          label={t.t('settings.playback.autoplayNext')}
          checked={playback.autoplay_next}
          onChange={(autoplay_next) => {
            update({ playback: { autoplay_next } });
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.playback.resume')}>
        <Switch
          label={t.t('settings.playback.resume')}
          checked={playback.resume_playback}
          onChange={(resume_playback) => {
            update({ playback: { resume_playback } });
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.playback.seekStep')} htmlFor={seekId}>
        <Slider
          id={seekId}
          value={playback.seek_step_seconds}
          min={1}
          max={30}
          format={(value) => `${value}s`}
          onChange={(seek_step_seconds) => {
            update({ playback: { seek_step_seconds } });
          }}
        />
      </SettingRow>

      {capabilities.playback_rate && (
        <SettingRow label={t.t('settings.playback.speed')} htmlFor={speedId}>
          <Slider
            id={speedId}
            value={playback.speed}
            min={0.25}
            max={2}
            step={0.25}
            format={(value) => `${value}×`}
            onChange={(speed) => {
              update({ playback: { speed } });
            }}
          />
        </SettingRow>
      )}

      {capabilities.caption_control && (
        <SettingRow label={t.t('settings.playback.captions')}>
          <Switch
            label={t.t('settings.playback.captions')}
            checked={playback.captions_enabled}
            onChange={(captions_enabled) => {
              update({ playback: { captions_enabled } });
            }}
          />
        </SettingRow>
      )}

      {/* Which control bar the player wears. Both options are real and neither is simply better,
          so the hint states the trade rather than nudging (ADR-0004). */}
      <SettingRow
        label={t.t('player.controls')}
        hint={t.t('player.controlsHint')}
        htmlFor={controlsId}
      >
        <Select
          id={controlsId}
          value={playback.player_controls}
          options={[
            { value: 'beastube', label: t.t('player.controlsBeastube') },
            { value: 'youtube', label: t.t('player.controlsYoutube') },
          ]}
          onChange={(player_controls) => {
            update({ playback: { player_controls } });
          }}
        />
      </SettingRow>

      {/* A ceiling on *automatic* selection, which is what the setting has always meant: it stops
          `Auto` climbing to 4K on a metered connection. Choosing a tier by hand in the player is a
          deliberate act and goes as high as the video offers, exactly as it does on YouTube.

          Still capability-gated, because an adapter that cannot select a tier cannot honour a
          ceiling either — the control is then absent rather than inert (§131, ADR-0004). */}
      {capabilities.quality_selection && (
        <SettingRow label={t.t('settings.playback.maxQuality')} htmlFor={maxQualityId}>
          <Select
            id={maxQualityId}
            value={playback.max_quality}
            options={QUALITY_TIERS.map((tier) => ({ value: tier, label: tier }))}
            onChange={(max_quality) => {
              update({ playback: { max_quality } });
            }}
          />
        </SettingRow>
      )}
    </SettingsSection>
  );
}

/**
 * Downloads.
 *
 * This panel exists mostly to make a dependency visible. BEASTUBE saves videos by running
 * `yt-dlp`, which is not shipped with it (ADR-0003), so the one question a user needs answered is
 * "is it there?" — and the answer, with the path and version behind it, is the first thing here.
 * The same goes for `ffmpeg`: without it downloads are capped at about 720p, which is stated
 * rather than left to be discovered from the files.
 *
 * Nothing here fetches or installs anything. The tools are the user's to install, and the panel
 * only ever reports and points.
 */
function DownloadsPanel(): ReactNode {
  const t = useTranslation();
  const downloads = useSettingsStore((state) => state.settings.downloads);
  const update = useSettingsStore((state) => state.update);
  const tools = useDownloadsStore((state) => state.tools);
  const refreshTools = useDownloadsStore((state) => state.refreshTools);
  const byVideo = useDownloadsStore((state) => state.byVideo);
  const [busy, setBusy] = useState(false);

  const qualityId = useControlId('download-quality');

  /** Runs a picker, stores what came back, and re-reads the local setup. */
  const pick = (choose: () => Promise<string | null>, apply: (path: string) => void) => {
    setBusy(true);
    void choose()
      .then((path) => {
        // `null` is the user cancelling, which must leave the setting exactly as it was.
        if (path !== null) apply(path);
      })
      .catch(() => {
        // The picker failed to open. Nothing changed, and a message about a dialog that did not
        // appear is less use than the unchanged row the user is looking at.
      })
      .finally(() => {
        // Deliberately after the write: the settings store persists on a debounce, but the native
        // side reads the in-memory document, which is already updated.
        void refreshTools().finally(() => {
          setBusy(false);
        });
      });
  };

  const downloaderStatus = (): string => {
    if (!tools?.downloader_path) return t.t('settings.downloads.downloaderMissing');
    return tools.downloader_version === null
      ? t.t('settings.downloads.downloaderNoVersion')
      : t.t('settings.downloads.downloaderFound', { version: tools.downloader_version });
  };

  const session = Object.values(byVideo).sort((a, b) => b.updated_at - a.updated_at);

  return (
    <SettingsSection
      title={t.t('settings.downloads.title')}
      description={t.t('settings.downloads.subtitle')}
    >
      <SettingRow label={t.t('settings.downloads.folder')}>
        <div className="flex flex-wrap items-center justify-end gap-2">
          <ReadOnlyValue value={tools?.directory ?? downloads.directory ?? '…'} mono />
          <SecondaryButton
            disabled={busy}
            onClick={() => {
              pick(
                () => invoke('pick_download_directory', undefined),
                (directory) => {
                  update({ downloads: { directory } });
                },
              );
            }}
          >
            {t.t('settings.downloads.changeFolder')}
          </SecondaryButton>
          <SecondaryButton
            onClick={() => {
              void invoke('open_download_directory', undefined).catch(() => {
                // The folder could not be opened. There is nothing to recover to here.
              });
            }}
          >
            {t.t('settings.downloads.openFolder')}
          </SecondaryButton>
        </div>
      </SettingRow>

      {/* A real ceiling, unlike the playback one: the downloader does choose a format, so this
          control does something rather than being present and inert. */}
      <SettingRow label={t.t('settings.downloads.quality')} htmlFor={qualityId}>
        <Select
          id={qualityId}
          value={downloads.max_quality}
          options={QUALITY_TIERS.map((tier) => ({ value: tier, label: tier }))}
          onChange={(max_quality) => {
            update({ downloads: { max_quality } });
          }}
        />
      </SettingRow>

      <SettingRow
        label={t.t('settings.downloads.downloader')}
        // The path when there is one, and how to get one when there is not.
        hint={tools?.downloader_path ?? t.t('settings.downloads.downloaderMissingHint')}
      >
        <div className="flex flex-wrap items-center justify-end gap-2">
          <ReadOnlyValue value={downloaderStatus()} />
          <SecondaryButton
            disabled={busy}
            onClick={() => {
              pick(
                () => invoke('pick_downloader_executable', undefined),
                (tool_path) => {
                  update({ downloads: { tool_path } });
                },
              );
            }}
          >
            {t.t('settings.downloads.choose')}
          </SecondaryButton>
          {downloads.tool_path !== null && (
            <SecondaryButton
              disabled={busy}
              onClick={() => {
                update({ downloads: { tool_path: null } });
                void refreshTools();
              }}
            >
              {t.t('settings.downloads.clear')}
            </SecondaryButton>
          )}
        </div>
      </SettingRow>

      {/* The value column truncates, so it carries the verdict and the hint beside the label
          carries the reason — which is the part that has to be readable when a tool is missing. */}
      <SettingRow
        label={t.t('settings.downloads.ffmpeg')}
        hint={
          tools?.can_merge === true
            ? (tools.ffmpeg_path ?? '')
            : t.t('settings.downloads.ffmpegMissingHint')
        }
      >
        <ReadOnlyValue
          value={
            tools?.can_merge === true
              ? t.t('settings.downloads.ffmpegFound')
              : t.t('settings.downloads.ffmpegMissing')
          }
        />
      </SettingRow>

      <SettingRow
        label={t.t('settings.downloads.jsRuntime')}
        hint={
          tools?.js_runtime
            ? t.t('settings.downloads.jsRuntimeFoundHint')
            : t.t('settings.downloads.jsRuntimeMissingHint')
        }
      >
        <ReadOnlyValue
          value={
            tools?.js_runtime
              ? t.t('settings.downloads.jsRuntimeFound', { name: tools.js_runtime })
              : t.t('settings.downloads.jsRuntimeMissing')
          }
        />
      </SettingRow>

      <SettingRow label={t.t('settings.downloads.recheck')}>
        <SecondaryButton
          disabled={busy}
          onClick={() => {
            setBusy(true);
            void refreshTools().finally(() => {
              setBusy(false);
            });
          }}
        >
          {t.t('settings.downloads.recheck')}
        </SecondaryButton>
      </SettingRow>

      <SettingRow label={t.t('settings.downloads.active')}>
        {session.length === 0 ? (
          <ReadOnlyValue value={t.t('settings.downloads.noneYet')} />
        ) : (
          <div className="flex max-h-48 flex-col gap-1 overflow-y-auto text-right">
            {session.map((download) => (
              <span key={download.id} className="text-text-muted text-xs">
                <span className="text-text">{download.title}</span>
                {' — '}
                {t.t(`settings.downloads.status.${download.status}`)}
                {/* Only when the size was known: a percentage derived from nothing would be a
                    number the application invented (§131). */}
                {download.fraction !== undefined &&
                  !isTerminalDownload(download.status) &&
                  ` ${Math.round(download.fraction * 100)}%`}
              </span>
            ))}
          </div>
        )}
      </SettingRow>
    </SettingsSection>
  );
}

function PrivacyPanel(): ReactNode {
  const t = useTranslation();
  const privacy = useSettingsStore((state) => state.settings.privacy);
  const update = useSettingsStore((state) => state.update);
  const incognito = useSessionStore((state) => state.incognito);
  const setIncognito = useSessionStore((state) => state.setIncognito);
  const [busy, setBusy] = useState(false);

  const storage = useAsyncResource('storage-stats', () => invoke('get_storage_stats', undefined));

  const clearHistory = () => {
    setBusy(true);
    void invoke('clear_history', undefined)
      .then(() => {
        storage.reload();
      })
      .finally(() => {
        setBusy(false);
      });
  };

  const clearSearches = () => {
    setBusy(true);
    void invoke('clear_search_history', undefined)
      .then(() => {
        storage.reload();
      })
      .finally(() => {
        setBusy(false);
      });
  };

  const clearCache = () => {
    setBusy(true);
    void invoke('clear_cache', undefined)
      .then(() => {
        // The in-memory caches too. Clearing only the native side left this session still holding
        // feeds and video metadata fetched before the click, so the button did not mean what it
        // said until the application was restarted.
        clearFeedCache();
        clearVideoCache();
        storage.reload();
      })
      .finally(() => {
        setBusy(false);
      });
  };

  return (
    <SettingsSection
      title={t.t('settings.privacy.title')}
      description={t.t('settings.privacy.subtitle')}
    >
      <SettingRow label={t.t('incognito.title')} hint={t.t('incognito.description')}>
        <Switch
          label={t.t('incognito.title')}
          checked={incognito}
          onChange={(enabled) => {
            void setIncognito(enabled);
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.privacy.history')}>
        <Switch
          label={t.t('settings.privacy.history')}
          checked={privacy.history_enabled}
          onChange={(history_enabled) => {
            update({ privacy: { history_enabled } });
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.privacy.searchHistory')}>
        <Switch
          label={t.t('settings.privacy.searchHistory')}
          checked={privacy.search_history_enabled}
          onChange={(search_history_enabled) => {
            update({ privacy: { search_history_enabled } });
          }}
        />
      </SettingRow>

      <SettingRow
        label={t.t('settings.privacy.recommendations')}
        hint={t.t('settings.privacy.recommendationsHint')}
      >
        <Switch
          label={t.t('settings.privacy.recommendations')}
          checked={privacy.local_recommendations_enabled}
          onChange={(local_recommendations_enabled) => {
            update({ privacy: { local_recommendations_enabled } });
          }}
        />
      </SettingRow>

      {/* Usage reporting is not a setting because there is nothing to switch: no telemetry code
          exists. It is stated here so the absence is visible rather than merely claimed. */}
      <SettingRow label={t.t('settings.privacy.telemetry')}>
        <ReadOnlyValue value={t.t('settings.privacy.telemetryValue')} />
      </SettingRow>

      <SettingRow label={t.t('settings.privacy.databaseLocation')}>
        <ReadOnlyValue value={storage.data?.database_path ?? '…'} mono />
      </SettingRow>

      <SettingRow label={t.t('settings.privacy.cacheLocation')}>
        <ReadOnlyValue value={storage.data?.cache_path ?? '…'} mono />
      </SettingRow>

      <SettingRow label={t.t('settings.privacy.storedData')}>
        <ReadOnlyValue
          value={
            storage.data
              ? [
                  `${t.bytes(storage.data.database_bytes)} library`,
                  `${t.bytes(storage.data.cache_bytes)} cache`,
                  t.plural('library.itemCount', storage.data.history_entries),
                ].join(' · ')
              : '…'
          }
        />
      </SettingRow>

      <SettingRow label={t.t('settings.privacy.clearCache')}>
        <SecondaryButton onClick={clearCache} disabled={busy}>
          {t.t('settings.privacy.clearCache')}
        </SecondaryButton>
      </SettingRow>

      <SettingRow label={t.t('settings.privacy.clearSearches')}>
        <DangerButton onClick={clearSearches} disabled={busy}>
          {t.t('settings.privacy.clearSearches')}
        </DangerButton>
      </SettingRow>

      <SettingRow label={t.t('settings.privacy.clearHistory')}>
        <DangerButton onClick={clearHistory} disabled={busy}>
          {t.t('settings.privacy.clearHistory')}
        </DangerButton>
      </SettingRow>
    </SettingsSection>
  );
}

/**
 * What the update control is doing right now.
 *
 * A tagged union rather than a set of booleans, because the states are genuinely exclusive and the
 * combinations a boolean set would allow — checking *and* installing, found *and* current — are all
 * meaningless.
 */
type UpdateState =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'current' }
  | { kind: 'found'; update: Update }
  | { kind: 'installing'; percent: number | null }
  | { kind: 'restarting' }
  | { kind: 'failed' };

/**
 * About, and the one control behind it that does real work.
 *
 * ## The update is one press, not a download page
 *
 * BEASTUBE ships as an installer, and an update is the next installer: fetched, signature-checked
 * against the key compiled into this binary, run without a wizard, then the application restarts.
 * From here that is a single button.
 *
 * What it is not is a patch — there is no delta mechanism on this path, so the whole application
 * comes down each time. The row says so before the press rather than after it, because fifty
 * megabytes is worth knowing about in advance.
 *
 * ## The strings for this existed for a long time before the control did
 *
 * `settings.about.checkUpdates` and its siblings were in the catalogue with nothing behind them,
 * and the updater plugin was a declared dependency that was never registered. A label that looks
 * like a feature and does nothing is precisely what §131 forbids, so either the plumbing had to
 * arrive or the strings had to go. This is the plumbing.
 */
function AboutPanel(): ReactNode {
  const t = useTranslation();
  const info = useAsyncResource('app-info', () => invoke('get_app_info', undefined));
  const [state, setState] = useState<UpdateState>({ kind: 'idle' });

  const check = () => {
    setState({ kind: 'checking' });
    void checkForUpdate().then(
      (update) => {
        setState(update === null ? { kind: 'current' } : { kind: 'found', update });
      },
      () => {
        // Offline, or the release feed is unreachable. Ordinary rather than exceptional: the row
        // says so and the button stays pressable.
        setState({ kind: 'failed' });
      },
    );
  };

  const install = (update: Update) => {
    setState({ kind: 'installing', percent: 0 });
    void downloadAndInstallUpdate(update, (percent) => {
      setState({ kind: 'installing', percent });
    }).then(
      () => {
        // The installer normally takes the process down itself; reaching here means it handed
        // control back instead, and the restart is ours to do.
        setState({ kind: 'restarting' });
        void relaunchApp();
      },
      () => {
        setState({ kind: 'failed' });
      },
    );
  };

  // Always a string: `exactOptionalPropertyTypes` distinguishes an absent prop from one explicitly
  // set to undefined, and every state here genuinely has something to say.
  const hint = (): string => {
    switch (state.kind) {
      case 'idle':
        return t.t('settings.about.updateSize');
      case 'current':
        return t.t('settings.about.upToDate');
      case 'found':
        return t.t('settings.about.updateAvailable', { version: state.update.version });
      case 'installing':
        return state.percent === null
          ? t.t('settings.about.installingUnknown')
          : t.t('settings.about.installing', { percent: String(state.percent) });
      case 'restarting':
        return t.t('settings.about.restarting');
      case 'failed':
        return t.t('settings.about.updateFailed');
      default:
        return t.t('settings.about.updateSize');
    }
  };

  const busy =
    state.kind === 'checking' || state.kind === 'installing' || state.kind === 'restarting';

  return (
    <SettingsSection title={t.t('settings.about.title')}>
      <SettingRow label={t.t('settings.about.title')}>
        <ReadOnlyValue value={info.data?.app_version ?? '…'} mono />
      </SettingRow>

      <SettingRow label={t.t('settings.about.checkUpdates')} hint={hint()}>
        {state.kind === 'found' ? (
          <SecondaryButton
            onClick={() => {
              install(state.update);
            }}
          >
            {t.t('settings.about.downloadUpdate')}
          </SecondaryButton>
        ) : (
          <SecondaryButton disabled={busy} onClick={check}>
            {busy ? t.t('app.loading') : t.t('settings.about.checkUpdates')}
          </SecondaryButton>
        )}
      </SettingRow>

      {/* Stated rather than linked: the licences are files the installer places beside the
          application, and naming them is what the obligation actually requires. */}
      <SettingRow label={t.t('settings.about.licenses')} hint={t.t('settings.about.licensesHint')}>
        <ReadOnlyValue value="binaries/licenses" mono />
      </SettingRow>
    </SettingsSection>
  );
}

function FilteringPanel(): ReactNode {
  const t = useTranslation();
  const filtering = useSettingsStore((state) => state.settings.filtering);
  const update = useSettingsStore((state) => state.update);
  const modeId = useControlId('filter-mode');

  const diagnostics = useAsyncResource('filtering-diagnostics', () =>
    invoke('get_filtering_diagnostics', undefined),
  );

  const modeHint: Record<FilteringMode, string> = {
    off: t.t('settings.filtering.modeOffHint'),
    standard: t.t('settings.filtering.modeStandardHint'),
    strict: t.t('settings.filtering.modeStrictHint'),
  };

  return (
    <SettingsSection
      title={t.t('settings.filtering.title')}
      description={t.t('settings.filtering.scopeNotice')}
    >
      <SettingRow label={t.t('settings.filtering.enabled')}>
        <Switch
          label={t.t('settings.filtering.enabled')}
          checked={filtering.enabled}
          onChange={(enabled) => {
            update({ filtering: { enabled } });
            diagnostics.reload();
          }}
        />
      </SettingRow>

      <SettingRow
        label={t.t('settings.filtering.mode')}
        hint={modeHint[filtering.mode]}
        htmlFor={modeId}
      >
        <Select
          id={modeId}
          value={filtering.mode}
          options={[
            { value: 'off' as const, label: t.t('settings.filtering.modeOff') },
            { value: 'standard' as const, label: t.t('settings.filtering.modeStandard') },
            { value: 'strict' as const, label: t.t('settings.filtering.modeStrict') },
          ]}
          onChange={(mode) => {
            update({ filtering: { mode } });
            diagnostics.reload();
          }}
        />
      </SettingRow>

      <SettingRow label={t.t('diagnostics.ruleCount')}>
        <ReadOnlyValue value={String(diagnostics.data?.counts.total ?? 0)} />
      </SettingRow>

      <SettingRow label={t.t('diagnostics.ruleVersion')}>
        <ReadOnlyValue value={diagnostics.data?.active_rule_version ?? '…'} mono />
      </SettingRow>

      {/* Counts only — never which requests were seen (§99). */}
      <SettingRow label={t.t('settings.filtering.blockedCount')}>
        <ReadOnlyValue
          value={`${t.number(diagnostics.data?.blocked ?? 0)} / ${t.number(
            diagnostics.data?.evaluated ?? 0,
          )}`}
        />
      </SettingRow>

      <SettingRow label={t.t('settings.filtering.resetRules')}>
        <SecondaryButton
          onClick={() => {
            void invoke('reset_filter_rules', undefined).then(() => {
              diagnostics.reload();
            });
          }}
        >
          {t.t('settings.filtering.resetRules')}
        </SecondaryButton>
      </SettingRow>
    </SettingsSection>
  );
}

/** The settings screen. */
export function SettingsView(): ReactNode {
  const t = useTranslation();

  return (
    <div className="mx-auto max-w-3xl">
      <h1 className="text-text mb-6 text-xl font-medium">{t.t('settings.title')}</h1>
      <AppearancePanel />
      <PlaybackPanel />
      <DownloadsPanel />
      <PrivacyPanel />
      <FilteringPanel />
      <AboutPanel />
    </div>
  );
}

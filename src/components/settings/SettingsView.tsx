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
import { invoke } from '@/services/ipc';
import { activePlaybackCapabilities } from '@/services/playback';
import { useSessionStore } from '@/stores/session';
import { useSettingsStore } from '@/stores/settings';
import type { Density, FilteringMode, Quality, Theme } from '@/types/domain';

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

      {/* The quality ceiling is only meaningful where the player can select a tier. Under the
          embedded player it cannot — `setPlaybackQuality` is a documented no-op — so the control is
          absent rather than present and inert (§131, ADR-0001). */}
      {capabilities.quality_selection ? (
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
      ) : (
        <SettingRow label={t.t('player.quality')}>
          <ReadOnlyValue value={t.t('player.qualityUnavailable')} />
        </SettingRow>
      )}
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
      <PrivacyPanel />
      <FilteringPanel />
    </div>
  );
}

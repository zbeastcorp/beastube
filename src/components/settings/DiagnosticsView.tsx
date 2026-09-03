/**
 * The diagnostics screen.
 *
 * Answers "what am I actually running, and what is it doing?" without a support channel and without
 * a network request. Everything shown is read from the local process; the copy button puts a plain
 * text report on the clipboard so the *user* decides where it goes (§121).
 *
 * Two rules shape what appears here:
 *
 * 1. **Absent, not blank.** A value the application cannot determine — the WebView2 runtime
 *    version, say — renders as "Unavailable" rather than as an empty row that looks like a bug.
 * 2. **Counts, never content.** The filtering rows are counters. A filtering layer sees every
 *    request, so a diagnostics screen that named one would amount to a browsing log kept in the
 *    subsystem best placed to build one (§99).
 */

import { useState, type ReactNode } from 'react';

import { SecondaryButton } from '@/components/common/Controls';
import { useAsyncResource } from '@/hooks/useAsyncResource';
import { useTranslation } from '@/i18n/context';
import type { Translator } from '@/i18n';
import { invoke, type AppInfo, type FilteringSnapshot, type StorageStats } from '@/services/ipc';
import { activePlaybackCapabilities } from '@/services/playback';
import { useSessionStore } from '@/stores/session';

/** One label/value pair. */
function Row({ label, value }: { label: string; value: string }): ReactNode {
  return (
    <div className="border-border flex items-start justify-between gap-6 border-b py-2.5 last:border-b-0">
      <span className="text-text-muted shrink-0 text-sm">{label}</span>
      <span className="text-text selectable min-w-0 text-right font-mono text-xs break-all">
        {value}
      </span>
    </div>
  );
}

function Group({ title, children }: { title: string; children: ReactNode }): ReactNode {
  return (
    <section className="mb-8">
      <h2 className="text-text mb-2 text-base font-medium">{title}</h2>
      <div className="bg-surface rounded-lg px-4">{children}</div>
    </section>
  );
}

/** Formats a millisecond duration as `2h 14m`, which reads better than `2:14:07` for an uptime. */
function formatUptime(millis: number): string {
  const totalMinutes = Math.floor(millis / 60_000);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  return hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
}

/**
 * Renders the report as plain text.
 *
 * Built from the same values on screen rather than from a second fetch, so what is copied is
 * exactly what was read.
 */
function buildReport(
  t: Translator,
  info: AppInfo | undefined,
  storage: StorageStats | undefined,
  filtering: FilteringSnapshot | undefined,
): string {
  const lines: string[] = ['BEASTUBE diagnostics'];
  if (info) {
    lines.push(
      `version: ${info.app_version} (${info.debug_build ? 'debug' : 'release'})`,
      `target: ${info.target}`,
      `webview: ${info.webview_version ?? 'unavailable'}`,
      `cores: ${info.cpu_cores}`,
      `uptime: ${formatUptime(info.uptime_ms)}`,
      `playback adapter: ${info.playback_adapter}`,
    );
  }
  if (filtering) {
    lines.push(
      `filtering: ${filtering.enabled ? filtering.mode : 'off'}`,
      `rule set: ${filtering.active_rule_version ?? 'none'} (${filtering.counts.total} rules)`,
      `evaluated/blocked: ${filtering.evaluated}/${filtering.blocked}`,
      `rejected updates: ${filtering.failed_updates}, rollbacks: ${filtering.rollbacks}`,
    );
  }
  if (storage) {
    lines.push(
      `database: ${t.bytes(storage.database_bytes)}`,
      `cache: ${t.bytes(storage.cache_bytes)}`,
      `history/bookmarks/positions: ${storage.history_entries}/${storage.bookmark_entries}/${storage.position_entries}`,
    );
  }
  return lines.join('\n');
}

/** The diagnostics screen. */
export function DiagnosticsView(): ReactNode {
  const t = useTranslation();
  const networkStatus = useSessionStore((state) => state.networkStatus);
  const capabilities = activePlaybackCapabilities();
  const [copied, setCopied] = useState(false);

  const info = useAsyncResource('app-info', () => invoke('get_app_info', undefined));
  const storage = useAsyncResource('diag-storage', () => invoke('get_storage_stats', undefined));
  const filtering = useAsyncResource('diag-filtering', () =>
    invoke('get_filtering_diagnostics', undefined),
  );

  const unavailable = t.t('diagnostics.unavailable');
  const supportedCapabilities = Object.entries(capabilities)
    .filter(([, supported]) => supported)
    .map(([name]) => name)
    .join(', ');

  const copyReport = () => {
    const report = buildReport(t, info.data, storage.data, filtering.data);
    void navigator.clipboard.writeText(report).then(
      () => {
        setCopied(true);
      },
      () => {
        // Clipboard access can be refused; saying nothing would look like the button did nothing.
        setCopied(false);
      },
    );
  };

  return (
    <div className="mx-auto max-w-3xl">
      <div className="mb-1 flex items-baseline justify-between gap-4">
        <h1 className="text-text text-xl font-medium">{t.t('diagnostics.title')}</h1>
        <SecondaryButton onClick={copyReport}>
          {copied ? t.t('app.copied') : t.t('diagnostics.copyReport')}
        </SecondaryButton>
      </div>
      <p className="text-text-muted mb-6 max-w-prose text-sm">{t.t('diagnostics.localOnly')}</p>

      <Group title={t.t('diagnostics.application')}>
        <Row label={t.t('diagnostics.version')} value={info.data?.app_version ?? '…'} />
        <Row
          label={t.t('diagnostics.build')}
          value={
            info.data
              ? t.t(info.data.debug_build ? 'diagnostics.buildDebug' : 'diagnostics.buildRelease')
              : '…'
          }
        />
        <Row
          label={t.t('diagnostics.uptime')}
          value={info.data ? formatUptime(info.data.uptime_ms) : '…'}
        />
        <Row label={t.t('diagnostics.provider')} value="youtube" />
      </Group>

      <Group title={t.t('diagnostics.system')}>
        <Row label={t.t('diagnostics.os')} value={info.data?.os ?? '…'} />
        <Row label={t.t('diagnostics.architecture')} value={info.data?.arch ?? '…'} />
        <Row
          label={t.t('diagnostics.cores')}
          value={info.data ? String(info.data.cpu_cores) : '…'}
        />
        <Row
          label={t.t('diagnostics.webviewVersion')}
          value={info.data ? (info.data.webview_version ?? unavailable) : '…'}
        />
      </Group>

      <Group title={t.t('diagnostics.playback')}>
        <Row
          label={t.t('diagnostics.playerAdapter')}
          value={info.data?.playback_adapter ?? 'iframe'}
        />
        <Row label={t.t('diagnostics.capabilities')} value={supportedCapabilities} />
        {/* Reported as unsupported rather than shown as zero: a zero would claim a measurement the
            adapter cannot make (§131). */}
        <Row
          label={t.t('diagnostics.bufferHealth')}
          value={capabilities.buffer_metrics ? '—' : t.t('diagnostics.unsupported')}
        />
        <Row
          label={t.t('diagnostics.droppedFrames')}
          value={capabilities.frame_metrics ? '—' : t.t('diagnostics.unsupported')}
        />
      </Group>

      <Group title={t.t('diagnostics.filtering')}>
        <Row
          label={t.t('settings.filtering.mode')}
          value={
            filtering.data?.enabled === true
              ? filtering.data.mode
              : t.t('settings.filtering.modeOff')
          }
        />
        <Row
          label={t.t('diagnostics.ruleVersion')}
          value={filtering.data?.active_rule_version ?? unavailable}
        />
        <Row
          label={t.t('diagnostics.ruleCount')}
          value={filtering.data ? String(filtering.data.counts.total) : '…'}
        />
        <Row
          label={t.t('settings.filtering.checksum')}
          value={filtering.data?.active_checksum ?? unavailable}
        />
        <Row
          label={t.t('diagnostics.evaluated')}
          value={t.number(filtering.data?.evaluated ?? 0)}
        />
        <Row label={t.t('diagnostics.blocked')} value={t.number(filtering.data?.blocked ?? 0)} />
        <Row
          label={t.t('diagnostics.neverBlocked')}
          value={t.number(filtering.data?.allowed_never_block ?? 0)}
        />
        <Row
          label={t.t('diagnostics.failedUpdates')}
          value={t.number(filtering.data?.failed_updates ?? 0)}
        />
        <Row
          label={t.t('diagnostics.rollbackState')}
          value={t.t(
            filtering.data?.rolled_back === true
              ? 'diagnostics.rolledBack'
              : 'diagnostics.notRolledBack',
          )}
        />
      </Group>

      <Group title={t.t('diagnostics.storage')}>
        <Row
          label={t.t('diagnostics.databaseSize')}
          value={storage.data ? t.bytes(storage.data.database_bytes) : '…'}
        />
        <Row
          label={t.t('diagnostics.cacheSize')}
          value={storage.data ? t.bytes(storage.data.cache_bytes) : '…'}
        />
        <Row
          label={t.t('diagnostics.historyEntries')}
          value={storage.data ? t.number(storage.data.history_entries) : '…'}
        />
        <Row
          label={t.t('diagnostics.bookmarkEntries')}
          value={storage.data ? t.number(storage.data.bookmark_entries) : '…'}
        />
        <Row
          label={t.t('diagnostics.positionEntries')}
          value={storage.data ? t.number(storage.data.position_entries) : '…'}
        />
        <Row
          label={t.t('settings.privacy.databaseLocation')}
          value={storage.data?.database_path ?? '…'}
        />
        <Row
          label={t.t('settings.privacy.cacheLocation')}
          value={storage.data?.cache_path ?? '…'}
        />
      </Group>

      <Group title={t.t('diagnostics.network')}>
        <Row
          label={t.t('diagnostics.network')}
          value={t.t(networkStatus === 'online' ? 'app.online' : 'app.offline')}
        />
      </Group>
    </div>
  );
}

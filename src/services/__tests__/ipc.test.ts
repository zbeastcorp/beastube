/**
 * The automatic retry at the IPC boundary.
 *
 * These exist because the behaviour they cover was, for a long time, declared and not implemented:
 * the error type has always carried `retry_automatic` with a delay and an attempt budget, and
 * nothing acted on it. A transient connect failure — the single most common way this application
 * fails — therefore sat on screen until someone pressed Retry, for a condition that would have
 * resolved itself in under a second.
 *
 * The tests are written against the *contract in the payload* rather than against fixed numbers, so
 * a change to the provider's stated delay or budget flows through rather than breaking them.
 */

import { afterEach, describe, expect, it, vi } from 'vitest';

import { invoke, setIpcMock } from '@/services/ipc';
import type { ErrorPayload } from '@/types/domain';

/** The budget the provider states for a connect failure. Mirrors `ProviderError::Transport`. */
const transportRecovery = {
  strategy: 'retry_automatic',
  delay_ms: 800,
  attempts_made: 1,
  max_attempts: 3,
} as const;

/** What the provider produces for a failed connection. */
const transportFailure: ErrorPayload = {
  kind: 'network',
  code: 'network.connect_failed',
  message_key: 'error.network.connect_failed',
  recovery: transportRecovery,
};

/** A failure the contract says is worth showing immediately. */
const schemaDrift: ErrorPayload = {
  kind: 'provider',
  code: 'provider.schema_drift',
  message_key: 'error.provider.schema_drift',
  recovery: { strategy: 'unrecoverable' },
};

afterEach(() => {
  setIpcMock(null);
  vi.useRealTimers();
});

/** Runs `body` with timers faked, draining the retry delays rather than waiting them out. */
async function withoutWaiting<T>(body: () => Promise<T>): Promise<T> {
  vi.useFakeTimers();
  const running = body();

  // Captured now, not awaited later. Advancing the timers is what lets the call settle, so a
  // rejection lands *during* the advance below — and a rejected promise with nothing attached to it
  // is reported as unhandled even when the very next line would have awaited it.
  const settled = running.then(
    (value) => ({ ok: true as const, value }),
    (error: unknown) => ({ ok: false as const, error }),
  );

  // Generously past the whole budget: two delays of 800ms and 1600ms at the current contract.
  await vi.advanceTimersByTimeAsync(10_000);

  const outcome = await settled;
  if (!outcome.ok) throw outcome.error;
  return outcome.value;
}

describe('invoke retries what the error says is retryable', () => {
  it('recovers from a transient failure without the caller ever seeing it', async () => {
    let calls = 0;
    setIpcMock(async () => {
      calls += 1;
      if (calls === 1) throw transportFailure;
      return { videos: [], source: 'discover' } as never;
    });

    const result = await withoutWaiting(() => invoke('get_recommended', { limit: 1 }));

    expect(calls).toBe(2);
    expect(result).toEqual({ videos: [], source: 'discover' });
  });

  it('gives up after the attempt budget the payload states, rather than forever', async () => {
    let calls = 0;
    setIpcMock(async () => {
      calls += 1;
      throw transportFailure;
    });

    await expect(withoutWaiting(() => invoke('get_recommended', { limit: 1 }))).rejects.toThrow();

    // `max_attempts` counts the first try, so three total and not three *extra*. Getting this wrong
    // is how a retry becomes a stampede.
    expect(calls).toBe(transportRecovery.max_attempts);
  });

  it('does not retry a failure that retrying cannot fix', async () => {
    let calls = 0;
    setIpcMock(async () => {
      calls += 1;
      throw schemaDrift;
    });

    await expect(invoke('get_recommended', { limit: 1 })).rejects.toThrow();
    expect(calls).toBe(1);
  });

  it('surfaces a wait too long to sit through instead of hiding it', async () => {
    // What rate limiting looks like: the provider asks for thirty seconds. Waiting that out behind
    // the user's back would leave a screen on a skeleton with no explanation, so it is shown along
    // with the manual retry the error already offers.
    const rateLimited: ErrorPayload = {
      ...transportFailure,
      code: 'network.rate_limited',
      recovery: {
        strategy: 'retry_automatic',
        delay_ms: 30_000,
        attempts_made: 1,
        max_attempts: 2,
      },
    };
    let calls = 0;
    setIpcMock(async () => {
      calls += 1;
      throw rateLimited;
    });

    await expect(invoke('get_recommended', { limit: 1 })).rejects.toThrow();
    expect(calls).toBe(1);
  });

  it('abandons the retry when the caller stops caring', async () => {
    const controller = new AbortController();
    let calls = 0;
    setIpcMock(async () => {
      calls += 1;
      controller.abort();
      throw transportFailure;
    });

    await expect(
      withoutWaiting(() => invoke('get_recommended', { limit: 1 }, { signal: controller.signal })),
    ).rejects.toThrow();

    // Aborted during the first failure, so the delay is never waited out and no second request is
    // made. A cancelled screen must not keep talking to the network.
    expect(calls).toBe(1);
  });
});

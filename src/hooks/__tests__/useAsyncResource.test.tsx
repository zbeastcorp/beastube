/**
 * The rule these cover is the one that is easy to state and was easy to get wrong: a result belongs
 * to the request that produced it.
 *
 * `data` is deliberately retained across a key change, so a refetch does not blank the screen. An
 * *error* is not, and the difference is not academic — it shipped. Once any fetch had failed, the
 * failure kept being reported for every later key, and because `data` was still undefined the views
 * that gate on `error && !data` showed "Something went wrong" for a second on every video opened
 * afterwards, until that video's own metadata arrived.
 */

import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { useAsyncResource } from '@/hooks/useAsyncResource';

describe('useAsyncResource', () => {
  it('reports a failure for the request that produced it', async () => {
    const { result } = renderHook(() =>
      useAsyncResource('one', () => Promise.reject(new Error('boom'))),
    );

    await waitFor(() => {
      expect(result.current.error).not.toBeNull();
    });
    expect(result.current.loading).toBe(false);
  });

  it('does not carry a failure over to the next request', async () => {
    const fetcher = vi.fn((key: string) =>
      key === 'bad' ? Promise.reject(new Error('boom')) : Promise.resolve('fine'),
    );

    const { result, rerender } = renderHook(
      ({ key }) => useAsyncResource(key, () => fetcher(key)),
      {
        initialProps: { key: 'bad' },
      },
    );

    await waitFor(() => {
      expect(result.current.error).not.toBeNull();
    });

    // The moment the key changes, the previous request's failure must stop being reported — even
    // though its result is the only one that has settled.
    rerender({ key: 'good' });
    expect(result.current.error).toBeNull();
    expect(result.current.loading).toBe(true);

    await waitFor(() => {
      expect(result.current.data).toBe('fine');
    });
    expect(result.current.error).toBeNull();
  });

  it('shows loading rather than the old failure while retrying', async () => {
    let attempt = 0;
    const { result } = renderHook(() =>
      useAsyncResource('same', () => {
        attempt += 1;
        return attempt === 1 ? Promise.reject(new Error('boom')) : Promise.resolve('recovered');
      }),
    );

    await waitFor(() => {
      expect(result.current.error).not.toBeNull();
    });

    act(() => {
      result.current.reload();
    });
    // A retry is a new request, so the failure it is busy retrying is no longer the current one.
    expect(result.current.error).toBeNull();

    await waitFor(() => {
      expect(result.current.data).toBe('recovered');
    });
  });

  it('keeps the previous value visible while the next request runs', async () => {
    const { result, rerender } = renderHook(
      ({ key }) => useAsyncResource(key, () => Promise.resolve(key)),
      {
        initialProps: { key: 'first' },
      },
    );

    await waitFor(() => {
      expect(result.current.data).toBe('first');
    });

    rerender({ key: 'second' });
    // Deliberately still the old value: blanking the screen on every navigation is worse than
    // briefly showing what was there, and callers that must not do this compare against the key.
    expect(result.current.data).toBe('first');
    expect(result.current.loading).toBe(true);

    await waitFor(() => {
      expect(result.current.data).toBe('second');
    });
  });
});

import {afterEach, expect, it, vi} from 'vitest';
import {act, cleanup, renderHook} from '@testing-library/react';
import {ApiError, InspectorClient} from '../../api/client';
import type {DetailReply} from '../../api/generated/detail';
import type {ListReply} from '../../api/generated/list';
import type {OutcomeReply} from '../../api/generated/outcome';
import type {PreviewReply} from '../../api/generated/preview';
import {detailFixture, listFixture, previewFixture, statsFixture} from '../../fixtures/contracts';
import {ReplayController, type ReplayApi} from '../replay/controller';
import {useHistory, type HistoryApi} from './useHistory';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return {promise, resolve, reject};
}
const summary = listFixture().items[0]!;
const other = {...summary, capture_id: '00000000000000000000000001'};
function historyApi(): HistoryApi {
  return {stats: vi.fn().mockResolvedValue(statsFixture()), list: vi.fn().mockResolvedValue(listFixture()), detail: vi.fn().mockResolvedValue(detailFixture())};
}
function replayApi(): ReplayApi {
  return {
    availability: vi.fn().mockResolvedValue({schema_version: 1, availability: {state: 'eligible', reason: null, raw_expires_at_unix_ms: 2_000, request_body_bytes: 0}}),
    prepare: vi.fn().mockResolvedValue(previewFixture()),
    confirm: vi.fn().mockResolvedValue({schema_version: 1, outcome: null}),
    outcome: vi.fn().mockResolvedValue({schema_version: 1, outcome: null}),
  };
}
async function flush() { await act(async () => {}); }
async function review(replay: ReplayController) {
  await replay.checkAvailability(); await replay.prepare(); replay.acknowledge(true);
  expect(replay.getSnapshot().phase).toBe('reviewing_acknowledged');
}
afterEach(() => { cleanup(); vi.useRealTimers(); });

it.each(['preparing', 'reviewing_acknowledged'] as const)('revokes %s on disconnected stats immediately while list is held, with no late preparation resurrection', async phase => {
  const service = historyApi();
  const list = deferred<ListReply>();
  service.list = vi.fn().mockReturnValue(list.promise);
  const stats = statsFixture(); stats.tunnel.connected = false;
  service.stats = vi.fn().mockResolvedValue(stats);
  const preparing = deferred<PreviewReply>(); const mutations = replayApi();
  if (phase === 'preparing') mutations.prepare = vi.fn().mockReturnValue(preparing.promise);
  const replay = new ReplayController(mutations, () => 0);
  const hook = renderHook(({paused}) => useHistory(service, {}, paused, () => replay.invalidate(), row => replay.select(row)), {initialProps: {paused: true}});
  act(() => hook.result.current.select(summary)); await flush();
  await replay.checkAvailability(); const pending = replay.prepare();
  if (phase === 'reviewing_acknowledged') { await pending; replay.acknowledge(true); }
  expect(replay.getSnapshot().phase).toBe(phase);
  act(() => hook.rerender({paused: false})); await flush();
  expect(hook.result.current.stats?.tunnel.connected).toBe(false);
  expect(replay.getSnapshot().phase).toBe('idle');
  preparing.resolve(previewFixture()); await pending;
  expect(replay.getSnapshot().review).toBeNull();
  replay.acknowledge(true); await replay.confirm(); expect(mutations.confirm).not.toHaveBeenCalled();
  list.resolve(listFixture()); await flush();
});

it('revokes an acknowledged review on a connected generation change before a held list settles', async () => {
  vi.useFakeTimers();
  const service = historyApi(); const mutations = replayApi();
  const replay = new ReplayController(mutations, () => 0);
  const invalidate = vi.fn(() => replay.invalidate());
  const hook = renderHook(() => useHistory(service, {}, false, invalidate, row => replay.select(row)));
  await flush(); act(() => hook.result.current.select(summary)); await flush(); await review(replay);
  const list = deferred<ListReply>(); service.list = vi.fn().mockReturnValue(list.promise);
  const next = statsFixture(); next.tunnel.generation = other.capture_id;
  service.stats = vi.fn().mockResolvedValue(next);
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(hook.result.current.stats?.tunnel.generation).toBe(other.capture_id);
  expect(replay.getSnapshot().phase).toBe('idle');
  expect(invalidate).toHaveBeenCalledTimes(1);
  await replay.confirm(); expect(mutations.confirm).not.toHaveBeenCalled();
  const old = detailFixture(); old.connection = 'superseded'; service.detail = vi.fn().mockResolvedValue(old);
  list.resolve(listFixture()); await flush();
  expect(hook.result.current.detail?.connection).toBe('superseded');
});

it('compares the first stats generation with the selected generation, not just previous polls', async () => {
  const service = historyApi(); const next = statsFixture(); next.tunnel.generation = other.capture_id;
  service.stats = vi.fn().mockResolvedValue(next);
  const invalidate = vi.fn();
  const hook = renderHook(({paused}) => useHistory(service, {}, paused, invalidate, () => {}), {initialProps: {paused: true}});
  act(() => hook.result.current.select(summary)); await flush();
  act(() => hook.rerender({paused: false})); await flush();
  // The stats mismatch and its current authoritative detail are independent
  // observations; each clears any authority acquired between them.
  expect(invalidate).toHaveBeenCalledTimes(2);
});

it.each(['superseded', 'disconnected'] as const)('revokes a review when selected detail becomes %s without resetting unchanged healthy polls', async connection => {
  vi.useFakeTimers();
  const service = historyApi(); const replay = new ReplayController(replayApi(), () => 0);
  const hook = renderHook(() => useHistory(service, {}, false, () => replay.invalidate(), row => replay.select(row)));
  await flush(); act(() => hook.result.current.select(summary)); await flush(); await review(replay);
  const changed = detailFixture(); changed.connection = connection;
  service.detail = vi.fn().mockResolvedValue(changed);
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(replay.getSnapshot().phase).toBe('idle');
  expect(hook.result.current.detail?.connection).toBe(connection);
});

it('preserves a foreground selected detail through pause cleanup instead of stranding loading', async () => {
  const service = historyApi(); const detail = deferred<DetailReply>();
  let signal: AbortSignal | undefined;
  service.detail = vi.fn().mockImplementation((_id: string, readSignal?: AbortSignal) => {
    signal = readSignal;
    signal?.addEventListener('abort', () => detail.reject(new ApiError('aborted')), {once: true});
    return detail.promise;
  });
  const hook = renderHook(({paused}) => useHistory(service, {}, paused, () => {}, () => {}), {initialProps: {paused: false}});
  await flush(); act(() => hook.result.current.select(summary));
  act(() => hook.rerender({paused: true}));
  expect(signal?.aborted).toBe(false);
  detail.resolve(detailFixture()); await flush();
  expect(hook.result.current.detailState).toBe('ready');
  expect(hook.result.current.detail?.capture_id).toBe(summary.capture_id);
});

it.each([404, 401])('stale A %i cannot evict B, but stale 401 revokes globally', async status => {
  const service = historyApi(); const first = deferred<DetailReply>();
  const second = detailFixture(); second.capture_id = other.capture_id;
  service.detail = vi.fn().mockReturnValueOnce(first.promise).mockResolvedValueOnce(second);
  const replay = new ReplayController(replayApi(), () => 0); const invalidate = vi.fn(() => replay.invalidate());
  const hook = renderHook(() => useHistory(service, {}, true, invalidate, row => replay.select(row)));
  act(() => hook.result.current.select(summary)); act(() => hook.result.current.select(other)); await flush();
  first.reject(new ApiError(status === 401 ? 'unauthorized' : 'not_found', status)); await flush();
  expect(hook.result.current.detailState).toBe('ready');
  if (status === 404) {
    expect(hook.result.current.detail?.capture_id).toBe(other.capture_id); expect(invalidate).not.toHaveBeenCalled();
  } else {
    expect(hook.result.current.detail).toBeNull(); expect(hook.result.current.forbidden).toBe(true); expect(invalidate).toHaveBeenCalledTimes(1);
  }
});

it('cannot resurrect a preparation completed after selected capture eviction', async () => {
  const service = historyApi(); const mutations = replayApi(); const preparing = deferred<PreviewReply>();
  mutations.prepare = vi.fn().mockReturnValue(preparing.promise);
  const replay = new ReplayController(mutations, () => 0);
  const hook = renderHook(({paused}) => useHistory(service, {}, paused, () => replay.invalidate(), row => replay.select(row)), {initialProps: {paused: true}});
  act(() => hook.result.current.select(summary)); await flush();
  await replay.checkAvailability(); const pending = replay.prepare();
  service.detail = vi.fn().mockRejectedValue(new ApiError('not_found', 404));
  act(() => hook.rerender({paused: false})); await flush();
  expect(hook.result.current.detailState).toBe('evicted');
  preparing.resolve(previewFixture()); await pending;
  expect(replay.getSnapshot().review).toBeNull(); expect(replay.getSnapshot().phase).toBe('idle');
});

it('keeps review, acknowledgment, pending confirmation and outcome through actual timer-driven polls and transient 503', async () => {
  vi.useFakeTimers();
  const service = historyApi(); const mutations = replayApi(); const confirmation = deferred<OutcomeReply>();
  mutations.confirm = vi.fn().mockReturnValue(confirmation.promise);
  const replay = new ReplayController(mutations, () => 0); const invalidate = vi.fn(() => replay.invalidate());
  const hook = renderHook(() => useHistory(service, {}, false, invalidate, row => replay.select(row)));
  await flush(); act(() => hook.result.current.select(summary)); await flush();
  await replay.checkAvailability(); await replay.prepare();
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(replay.getSnapshot().phase).toBe('reviewing');
  replay.acknowledge(true);
  service.detail = vi.fn().mockRejectedValueOnce(new ApiError('busy', 503)).mockResolvedValue(detailFixture());
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(replay.getSnapshot().phase).toBe('reviewing_acknowledged'); expect(hook.result.current.detail).not.toBeNull();
  const pending = replay.confirm();
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(replay.getSnapshot().phase).toBe('submitting');
  confirmation.resolve({schema_version: 1, outcome: null}); await pending;
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(replay.getSnapshot().phase).toBe('outcome'); expect(replay.getSnapshot().attempt).not.toBeNull();
  await replay.refreshOutcome(); expect(mutations.outcome).toHaveBeenCalledTimes(1);
  expect(service.stats).toHaveBeenCalledTimes(5); expect(invalidate).not.toHaveBeenCalled(); expect(mutations.confirm).toHaveBeenCalledTimes(1);
});

it('bounds fetch-backed background reads to two plus one outcome and cancels detail before replacement launch', async () => {
  const events: string[] = []; let active = 0; let peak = 0; let background = 0; let backgroundPeak = 0;
  const requests: {path: string; resolve: (value: unknown) => void; holdBody: () => void}[] = [];
  const fetcher = vi.fn<typeof fetch>().mockImplementation((input, init) => {
    const path = String(input); const isBackground = !path.includes('/replays/');
    active++; peak = Math.max(peak, active);
    if (isBackground) { background++; backgroundPeak = Math.max(backgroundPeak, background); }
    events.push(`start ${path}`);
    return new Promise<Response>((resolve, reject) => {
      let done = false;
      const finish = (event: string) => {
        if (done) return false;
        done = true; active--; if (isBackground) background--;
        init?.signal?.removeEventListener('abort', abort); events.push(`${event} ${path}`); return true;
      };
      let body: ReadableStreamDefaultController<Uint8Array> | undefined;
      const abort = () => {
        if (!finish('abort')) return;
        const error = new DOMException('Aborted', 'AbortError');
        body?.error(error); reject(error);
      };
      const respond = (value: unknown, held: boolean) => {
        resolve(new Response(new ReadableStream<Uint8Array>({
          start(controller) { body = controller; },
          pull(controller) {
            if (held || done) return;
            controller.enqueue(new TextEncoder().encode(JSON.stringify(value))); controller.close(); finish('finish');
          },
          cancel() { finish('cancel'); },
        }), {headers: {'Content-Type': 'application/json'}}));
      };
      init?.signal?.addEventListener('abort', abort, {once: true});
      if (init?.signal?.aborted) abort();
      requests.push({path, resolve: value => respond(value, false), holdBody: () => respond(null, true)});
    });
  });
  const client = new InspectorClient('a'.repeat(64), fetcher);
  const hook = renderHook(({options, paused}) => useHistory(client, options, paused, () => {}, () => {}), {initialProps: {options: {}, paused: true}});
  act(() => hook.result.current.select(summary)); requests[0]!.resolve(detailFixture()); await flush();
  const outcome = client.outcome(summary.capture_id);
  act(() => hook.rerender({options: {}, paused: false}));
  requests.find(r => r.path.endsWith('/stats'))!.resolve(statsFixture());
  requests.find(r => r.path.includes('?'))!.resolve(listFixture()); await flush();
  // Deliver headers but hold the detail body, exercising cancellation after
  // fetch resolves as well as the still-pending explicit outcome fetch.
  requests.at(-1)!.holdBody(); await flush();
  expect(active).toBe(2);
  const heldDetail = requests.at(-1)!.path; const before = events.length;
  act(() => hook.rerender({options: {q: 'next'}, paused: false})); await flush();
  expect(events.slice(before, before + 3)).toEqual([`abort ${heldDetail}`, 'start /api/v1/stats', 'start /api/v1/requests?limit=50&q=next']);
  expect(backgroundPeak).toBe(2); expect(peak).toBe(3); expect(active).toBe(3);
  hook.unmount(); expect(active).toBe(1);
  requests.find(r => r.path.includes('/replays/'))!.resolve({schema_version: 1, outcome: null}); await outcome;
  expect(active).toBe(0);
});

it('revokes when detail generation no longer matches the selected summary', async () => {
  vi.useFakeTimers(); const service = historyApi(); const replay = new ReplayController(replayApi(), () => 0);
  const hook = renderHook(() => useHistory(service, {}, false, () => replay.invalidate(), row => replay.select(row)));
  await flush(); act(() => hook.result.current.select(summary)); await flush(); await review(replay);
  const changed = detailFixture(); changed.generation = other.capture_id; service.detail = vi.fn().mockResolvedValue(changed);
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(replay.getSnapshot().phase).toBe('idle');
});

it('revokes every unavailable selected detail, including a held preparation after fresh authority without a healthy detail', async () => {
  vi.useFakeTimers();
  const service = historyApi(); const mutations = replayApi(); const preparing = deferred<PreviewReply>();
  const replay = new ReplayController(mutations, () => 0);
  const hook = renderHook(() => useHistory(service, {}, false, () => replay.invalidate(), row => replay.select(row)));
  await flush(); act(() => hook.result.current.select(summary)); await flush(); await review(replay);
  const disconnected = detailFixture(); disconnected.connection = 'disconnected';
  service.detail = vi.fn().mockResolvedValue(disconnected);
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(replay.getSnapshot().phase).toBe('idle');
  await review(replay);
  mutations.prepare = vi.fn().mockReturnValue(preparing.promise);
  await replay.checkAvailability(); const pending = replay.prepare();
  expect(replay.getSnapshot().phase).toBe('preparing');
  await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
  expect(replay.getSnapshot().phase).toBe('idle');
  preparing.resolve(previewFixture()); await pending;
  expect(replay.getSnapshot().review).toBeNull();
});

it('treats a late superseded poll 401 as global revocation rather than a stale selection result', async () => {
  const service = historyApi(); const oldStats = deferred<ReturnType<typeof statsFixture>>();
  service.stats = vi.fn().mockReturnValueOnce(oldStats.promise).mockResolvedValue(statsFixture());
  const replay = new ReplayController(replayApi(), () => 0);
  const hook = renderHook(({options}) => useHistory(service, options, false, () => replay.invalidate(), row => replay.select(row)), {initialProps: {options: {}}});
  act(() => hook.result.current.select(summary)); await flush(); await review(replay);
  act(() => hook.rerender({options: {q: 'changed'}})); await flush();
  expect(replay.getSnapshot().phase).toBe('reviewing_acknowledged');
  oldStats.reject(new ApiError('unauthorized', 401)); await flush();
  expect(hook.result.current.forbidden).toBe(true); expect(replay.getSnapshot().review).toBeNull();
});

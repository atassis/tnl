import {useCallback, useEffect, useRef, useState} from 'react';
import {ApiError} from '../../api/client';
import type {InspectorClient, ListOptions} from '../../api/client';
import type {DetailReply} from '../../api/generated/detail';
import type {Summary, ListReply} from '../../api/generated/list';
import type {StatsReply} from '../../api/generated/stats';

export type HistoryApi = Pick<InspectorClient, 'stats' | 'list' | 'detail'>;
export function useHistory(api: HistoryApi, options: ListOptions, paused: boolean, invalidate: () => void, onSelect: (row: Summary) => void) {
  const [detail, setDetail] = useState<DetailReply | null>(null);
  const [detailState, setDetailState] = useState<'idle' | 'loading' | 'ready' | 'evicted' | 'busy' | 'error'>('idle');
  const [page, setPage] = useState<ListReply | null>(null);
  const [stats, setStats] = useState<StatsReply | null>(null);
  const [message, setMessage] = useState('');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [forbidden, setForbidden] = useState(false);
  const callbacks = useRef({invalidate, onSelect});
  callbacks.current = {invalidate, onSelect};
  const selected = useRef<string | null>(null);
  const mounted = useRef(true);
  const stopped = useRef(false);
  const tunnel = useRef<StatsReply['tunnel'] | null>(null);
  const selectedGeneration = useRef<string | null>(null);
  const detailEpoch = useRef(0);
  const detailAbort = useRef<AbortController | null>(null);
  const detailBusy = useRef(false);
  const detailBackground = useRef(false);
  const revokeSelection = useCallback(() => {
    callbacks.current.invalidate();
  }, []);
  const error = useCallback((value: unknown) => {
    const failure = value instanceof ApiError ? value : new ApiError('network');
    if (failure.kind === 'aborted') return;
    if (failure.kind === 'unauthorized') {
      stopped.current = true; detailEpoch.current++; detailAbort.current?.abort();
      callbacks.current.invalidate(); setForbidden(true); setDetail(null);
    }
    setMessage(failure.message);
  }, []);
  const readDetail = useCallback(async (captureId: string, background: boolean) => {
    if (stopped.current || (background && detailBusy.current)) return;
    const epoch = ++detailEpoch.current;
    detailAbort.current?.abort();
    const controller = new AbortController();
    detailAbort.current = controller; detailBusy.current = true; detailBackground.current = background;
    try {
      const value = await api.detail(captureId, controller.signal);
      if (!mounted.current || stopped.current || epoch !== detailEpoch.current || selected.current !== captureId) return;
      if (value.connection !== 'connected' || value.generation !== selectedGeneration.current ||
          (tunnel.current && (!tunnel.current.connected || tunnel.current.generation !== value.generation))) revokeSelection();
      setDetail(value); setDetailState('ready'); setMessage('');
    } catch (value) {
      if (!mounted.current) return;
      if (value instanceof ApiError && value.kind === 'unauthorized') { error(value); return; }
      if (epoch !== detailEpoch.current || selected.current !== captureId) return;
      if (value instanceof ApiError && value.kind === 'aborted') return;
      if (value instanceof ApiError && value.kind === 'not_found') {
        // Eviction revokes local replay authority before the UI can retain a
        // stale detail/review for a capture the server no longer has.
        callbacks.current.invalidate();
        setDetail(null); setDetailState('evicted'); error(value);
        return;
      }
      setDetailState(value instanceof ApiError && value.kind === 'busy' ? 'busy' : 'error');
      error(value);
    } finally {
      if (epoch === detailEpoch.current) detailBusy.current = false;
    }
  }, [api, error, revokeSelection]);
  const select = useCallback((row: Summary) => {
    if (stopped.current) return;
    callbacks.current.onSelect(row);
    selected.current = row.capture_id;
    selectedGeneration.current = row.generation;
    if (row.connection !== 'connected' ||
        (tunnel.current && (!tunnel.current.connected || tunnel.current.generation !== row.generation))) revokeSelection();
    setSelectedId(row.capture_id); setDetail(null); setDetailState('loading');
    void readDetail(row.capture_id, false);
  }, [readDetail, revokeSelection]);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; detailEpoch.current++; detailAbort.current?.abort(); detailBusy.current = false; };
  }, [api]);
  const queryKey = JSON.stringify(options);
  useEffect(() => {
    if (paused || stopped.current) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const controller = new AbortController();
    const schedule = () => { if (!disposed && !stopped.current) timer = setTimeout(() => { void cycle(); }, 2000); };
    async function cycle() {
      if (disposed || stopped.current) return;
      if (document.hidden) { schedule(); return; }
      // Wait for both reads to settle before another cycle/detail read. A fast
      // rejection must not leave a slow read overlapping the next poll pair.
      const watch = <T,>(read: Promise<T>) => read.catch(value => {
        if (mounted.current && value instanceof ApiError && value.kind === 'unauthorized') {
          error(value); controller.abort();
        }
        throw value;
      });
      const statsRead = watch(api.stats(controller.signal)).then(value => {
        if (disposed || stopped.current) return;
        const previous = tunnel.current;
        tunnel.current = value.tunnel;
        setStats(value);
        // Stats revoke authority on arrival, even if the paired list is held.
        // An unchanged connected generation must leave review/attempts intact.
        if ((previous && previous.generation !== value.tunnel.generation) ||
            (!value.tunnel.connected && previous?.connected !== false)) {
          callbacks.current.invalidate();
        } else if (selected.current && (!value.tunnel.connected || value.tunnel.generation !== selectedGeneration.current)) {
          revokeSelection();
        }
      });
      const results = await Promise.allSettled([statsRead, watch(api.list(options, controller.signal))]);
      if (disposed || stopped.current) return;
      const [, listResult] = results;
      for (const result of results) if (result.status === 'rejected') error(result.reason);
      if (stopped.current) return;
      if (listResult.status === 'fulfilled') setPage(listResult.value);
      if (selected.current) await readDetail(selected.current, true);
      schedule();
    }
    void cycle();
    return () => {
      disposed = true; clearTimeout(timer); controller.abort();
      // Only background refresh belongs to this effect. Explicit selection must
      // still complete when paused; cancel background detail before replacement
      // stats/list launch to preserve the two-background-read bound.
      if (detailBusy.current && detailBackground.current) {
        detailEpoch.current++; detailAbort.current?.abort(); detailAbort.current = null; detailBusy.current = false;
      }
    };
  }, [api, queryKey, paused, error, readDetail, revokeSelection]);
  return {detail, detailState, page, stats, message, selectedId, select, forbidden};
}

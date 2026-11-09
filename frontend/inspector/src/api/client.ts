import type { StatsReply } from './generated/stats';
import type { DetailReply } from './generated/detail';
import type { ListReply } from './generated/list';
import type { AvailabilityReply } from './generated/availability';
import type { PreviewReply } from './generated/preview';
import type { OutcomeReply } from './generated/outcome';
import type { ConfirmRequest } from './generated/confirm_request';
import type { ReplayError } from './generated/replay_error';
import { validate_stats, validate_detail, validate_list, validate_availability, validate_preview, validate_outcome, validate_replay_error } from './generated/validators';
import { validTiming } from '../features/timing/projection';

export type FailureKind = 'unauthorized' | 'not_found' | 'busy' | 'http' | 'invalid_response' | 'network' | 'aborted';
const messages: Record<FailureKind, string> = {
  unauthorized: 'Inspector access expired. Reopen the private startup link.',
  not_found: 'Capture or outcome is no longer retained.',
  busy: 'Inspector is busy. No mutation was retried.',
  http: 'Inspector rejected the request. No automatic mutation retry.',
  invalid_response: 'Inspector returned an invalid or oversized response.',
  network: 'Inspector is unavailable or the request timed out. Delivery may be unknown.',
  aborted: 'Inspector read cancelled.',
};
export class ApiError extends Error {
  constructor(readonly kind: FailureKind, readonly status = 0, readonly replayReason: ReplayError | null = null) {
    super(messages[kind]);
  }
}
type Validator<T> = (value: unknown) => value is T;
export interface ListOptions { before?: number; q?: string; method?: string; status?: string; state?: string }
const idPattern = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/;
function id(value: string): string {
  if (!idPattern.test(value)) throw new ApiError('invalid_response');
  return value;
}
function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const abort = () => reject(new ApiError('aborted'));
    if (signal.aborted) { abort(); return; }
    signal.addEventListener('abort', abort, { once: true });
    promise.then(value => { signal.removeEventListener('abort', abort); resolve(value); },
      () => { signal.removeEventListener('abort', abort); reject(new ApiError('network')); });
  });
}
async function json(response: Response, signal: AbortSignal): Promise<unknown> {
  if (response.headers.get('Content-Type')?.split(';')[0]?.trim() !== 'application/json' || !response.body) throw new ApiError('invalid_response');
  const reader = response.body.getReader();
  const bytes = new Uint8Array(65_536);
  let used = 0;
  try {
    for (;;) {
      const chunk = await abortable(reader.read(), signal);
      if (chunk.done) break;
      if (chunk.value.length > bytes.length - used) throw new ApiError('invalid_response');
      bytes.set(chunk.value, used); used += chunk.value.length;
    }
    try { const value: unknown = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, used))); return value; }
    catch { throw new ApiError('invalid_response'); }
  } finally {
    void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
export class InspectorClient {
  #token: string | null;
  constructor(token: string | null, private readonly fetcher: typeof fetch = globalThis.fetch.bind(globalThis)) {
    this.#token = token !== null && /^[0-9a-f]{64}$/.test(token) ? token : null;
  }
  revoke(): void { this.#token = null; }
  private async request<T>(path: string, validate: Validator<T>, signal?: AbortSignal, body?: unknown): Promise<T> {
    if (!this.#token) throw new ApiError('unauthorized');
    const controller = new AbortController();
    const cancel = () => controller.abort();
    signal?.addEventListener('abort', cancel, { once: true });
    if (signal?.aborted) cancel();
    const timer = setTimeout(cancel, 3000);
    let received: Response | undefined;
    try {
      if (controller.signal.aborted) throw new ApiError('aborted');
      const headers: Record<string, string> = { Authorization: `Bearer ${this.#token}` };
      if (body !== undefined) headers['Content-Type'] = 'application/json';
      const response = await abortable(this.fetcher(path, {
        method: body === undefined ? 'GET' : 'POST', headers, cache: 'no-store', credentials: 'omit',
        redirect: 'error', mode: 'same-origin', referrerPolicy: 'no-referrer', signal: controller.signal,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      }), controller.signal);
      received = response;
      if (!response.ok) {
        if (response.status === 401) { this.revoke(); throw new ApiError('unauthorized', 401); }
        // Mutation refusals carry execution-admission evidence, including at
        // 404/503. GETs retain their generic status semantics below.
        if (body !== undefined && response.headers.get('Content-Type')?.startsWith('application/json')) {
          const value = await json(response, controller.signal);
          if (!validate_replay_error(value)) throw new ApiError('invalid_response', response.status);
          throw new ApiError('http', response.status, value.error);
        }
        if (response.status === 404) throw new ApiError('not_found', 404);
        if (response.status === 503) throw new ApiError('busy', 503);
        throw new ApiError('http', response.status);
      }
      const value = await json(response, controller.signal);
      if (!validate(value)) throw new ApiError('invalid_response');
      return value;
    } catch (error) {
      if (controller.signal.aborted) throw new ApiError(signal?.aborted ? 'aborted' : 'network');
      if (error instanceof ApiError) throw error;
      throw new ApiError('network');
    } finally {
      clearTimeout(timer); signal?.removeEventListener('abort', cancel);
      void received?.body?.cancel().catch(() => {});
    }
  }
  stats(signal?: AbortSignal): Promise<StatsReply> { return this.request('/api/v1/stats', validate_stats, signal); }
  list(options: ListOptions = {}, signal?: AbortSignal): Promise<ListReply> {
    const query = new URLSearchParams({ limit: '50' });
    if (options.before !== undefined) {
      if (!Number.isSafeInteger(options.before) || options.before < 1) return Promise.reject(new ApiError('invalid_response'));
      query.set('before', String(options.before));
    }
    for (const key of ['q', 'method', 'status', 'state'] as const) {
      const value = options[key]; if (value) query.set(key, value.slice(0, 256));
    }
    return this.request(`/api/v1/requests?${query}`, validate_list, signal);
  }
  async detail(captureId: string, signal?: AbortSignal): Promise<DetailReply> {
    const value = await this.request(`/api/v1/requests/${id(captureId)}`, validate_detail, signal);
    if (!validTiming(value.timing)) throw new ApiError('invalid_response');
    return value;
  }
  async availability(captureId: string, signal?: AbortSignal): Promise<AvailabilityReply> {
    return this.request(`/api/v1/requests/${id(captureId)}/replay`, validate_availability, signal);
  }
  async prepare(captureId: string, signal?: AbortSignal): Promise<PreviewReply> {
    return this.request(`/api/v1/requests/${id(captureId)}/replay/prepare`, validate_preview, signal, { schema_version: 1 });
  }
  async confirm(captureId: string, body: ConfirmRequest): Promise<OutcomeReply> {
    return this.request(`/api/v1/requests/${id(captureId)}/replay/confirm`, validate_outcome, undefined, body);
  }
  async outcome(replayId: string, signal?: AbortSignal): Promise<OutcomeReply> {
    return this.request(`/api/v1/replays/${id(replayId)}`, validate_outcome, signal);
  }
}

import {ApiError} from '../../api/client';
import type {InspectorClient} from '../../api/client';
import type {Summary} from '../../api/generated/list';
import type {ReplayAvailability} from '../../api/generated/availability';
import type {ReplayOutcome} from '../../api/generated/outcome';

export type ReplayApi = Pick<InspectorClient, 'availability' | 'prepare' | 'confirm' | 'outcome'>;
type ReplayPhase = 'idle' | 'preparing' | 'reviewing' | 'reviewing_acknowledged' | 'submitting' | 'outcome' | 'error';
interface SelectedCapture { readonly captureId: string; readonly generation: string; }
interface Ticket extends SelectedCapture {
  readonly confirmationToken: string;
  readonly replayId: string;
  readonly confirmationExpiresAt: number;
  readonly rawExpiresAt: number;
}
interface Attempt {
  readonly replayId: string;
  readonly parentCaptureId: string;
  readonly generation: string;
}
export interface Review {
  readonly method: string;
  readonly path: string;
  readonly target: string;
  readonly hostPolicy: string;
  readonly requestBytes: number;
  readonly expires: number;
  readonly rawExpires: number;
  readonly warnings: readonly [string, string, string];
}
export interface ReplayView {
  readonly phase: ReplayPhase;
  readonly busy: boolean;
  readonly acknowledged: boolean;
  readonly canRecoverReview: boolean;
  readonly availability: ReplayAvailability | null;
  readonly review: Review | null;
  readonly outcome: ReplayOutcome | null;
  readonly attempt: {readonly replayId: string; readonly parentCaptureId: string} | null;
  readonly message: string;
}

const initialView: ReplayView = Object.freeze({
  phase: 'idle', busy: false, acknowledged: false, canRecoverReview: false, availability: null, review: null,
  outcome: null, attempt: null, message: '',
});

/**
 * Owns the private, one-use confirmation ticket. Views intentionally expose
 * review metadata only; a ticket never crosses this controller boundary.
 */
export class ReplayController {
  #selected: SelectedCapture | null = null;
  #ticket: Ticket | null = null;
  #attempt: Attempt | null = null;
  #epoch = 0;
  #safeRead: AbortController | null = null;
  #listeners = new Set<() => void>();
  #view: ReplayView = initialView;

  constructor(private readonly api: ReplayApi, private readonly now: () => number = Date.now) {}

  getSnapshot = (): ReplayView => this.#view;
  subscribe = (listener: () => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  #publish(update: Partial<ReplayView>): void {
    this.#view = Object.freeze({...this.#view, ...update});
    for (const listener of this.#listeners) listener();
  }

  #clearAuthority(): void {
    this.#epoch++;
    this.#safeRead?.abort();
    this.#safeRead = null;
    this.#ticket = null;
    this.#attempt = null;
    this.#publish(initialView);
  }

  select(row: Summary): void {
    this.#clearAuthority();
    this.#selected = {captureId: row.capture_id, generation: row.generation};
  }

  /** Clears review authority synchronously; late responses are epoch-bound. */
  invalidate(): void {
    this.#clearAuthority();
  }

  #active(epoch: number, selected: SelectedCapture): boolean {
    return epoch === this.#epoch && this.#selected?.captureId === selected.captureId && this.#selected.generation === selected.generation;
  }

  #message(error: unknown): string {
    return error instanceof ApiError ? error.message : 'Inspector is unavailable. No replay was retried.';
  }

  #handleUnauthorized(error: unknown): boolean {
    if (!(error instanceof ApiError) || error.kind !== 'unauthorized') return false;
    this.#clearAuthority();
    this.#publish({phase: 'error', message: error.message});
    return true;
  }

  async checkAvailability(): Promise<void> {
    const selected = this.#selected;
    if (!selected || this.#view.busy || this.#attempt || this.#view.phase === 'submitting') return;
    const epoch = this.#epoch;
    this.#safeRead?.abort();
    const controller = new AbortController();
    this.#safeRead = controller;
    this.#publish({busy: true, message: ''});
    try {
      const reply = await this.api.availability(selected.captureId, controller.signal);
      if (!this.#active(epoch, selected)) return;
      this.#publish({phase: 'idle', busy: false, availability: reply.availability, canRecoverReview: false, message: ''});
    } catch (error) {
      if (this.#handleUnauthorized(error)) return;
      if (!this.#active(epoch, selected) || (error instanceof ApiError && error.kind === 'aborted')) return;
      this.#publish({phase: 'error', busy: false, canRecoverReview: !(error instanceof ApiError) || error.kind === 'network' || error.kind === 'busy', message: this.#message(error)});
    } finally {
      if (this.#safeRead === controller) this.#safeRead = null;
    }
  }

  async prepare(): Promise<void> {
    const selected = this.#selected;
    if (!selected || this.#view.busy || this.#attempt || this.#view.phase !== 'idle' || this.#view.availability?.state !== 'eligible') return;
    const epoch = this.#epoch;
    this.#ticket = null;
    this.#publish({phase: 'preparing', busy: true, acknowledged: false, review: null, outcome: null, attempt: null, message: ''});
    try {
      // Preparation creates review material only. It never calls confirm.
      const reply = await this.api.prepare(selected.captureId);
      const preview = reply.preview;
      if (!this.#active(epoch, selected)) return;
      if (preview.capture_id !== selected.captureId || preview.tunnel_generation !== selected.generation) {
        this.#publish({phase: 'error', busy: false, message: 'Replay preparation no longer matches the selected capture.'});
        return;
      }
      if (this.now() >= preview.confirmation_expires_at_unix_ms || this.now() >= preview.raw_expires_at_unix_ms) {
        this.#publish({phase: 'error', busy: false, canRecoverReview: true, message: 'Replay review expired before confirmation.'});
        return;
      }
      this.#ticket = {
        captureId: preview.capture_id, generation: preview.tunnel_generation, replayId: preview.replay_id,
        confirmationToken: preview.confirmation_token, confirmationExpiresAt: preview.confirmation_expires_at_unix_ms,
        rawExpiresAt: preview.raw_expires_at_unix_ms,
      };
      this.#publish({
        phase: 'reviewing', busy: false, acknowledged: false,
        review: {method: reply.method, path: reply.request_target, target: preview.target, hostPolicy: preview.host_policy,
          requestBytes: preview.request_body_bytes, expires: preview.confirmation_expires_at_unix_ms,
          rawExpires: preview.raw_expires_at_unix_ms, warnings: preview.warnings},
      });
    } catch (error) {
      if (this.#handleUnauthorized(error)) return;
      if (!this.#active(epoch, selected)) return;
      const transientRejection = error instanceof ApiError && error.kind === 'http' &&
        (error.replayReason === 'busy' || error.replayReason === 'overloaded' || error.replayReason === 'runtime_unavailable');
      this.#publish({phase: 'error', busy: false, canRecoverReview: transientRejection || !(error instanceof ApiError) || error.kind === 'network' || error.kind === 'busy', message: this.#message(error)});
    }
  }

  acknowledge(value: boolean): void {
    if (this.#view.phase !== 'reviewing' && this.#view.phase !== 'reviewing_acknowledged') return;
    if (!this.#ticket || this.#expired(this.#ticket)) {
      this.#expire();
      return;
    }
    this.#publish({phase: value ? 'reviewing_acknowledged' : 'reviewing', acknowledged: value});
  }

  #expired(ticket: Ticket): boolean {
    return this.now() >= ticket.confirmationExpiresAt || this.now() >= ticket.rawExpiresAt;
  }

  #expire(): void {
    this.#ticket = null;
    this.#publish({phase: 'error', busy: false, acknowledged: false, review: null, canRecoverReview: true, message: 'Replay review expired. Prepare a new review and acknowledge it again.'});
  }

  async confirm(): Promise<void> {
    if (this.#view.phase !== 'reviewing_acknowledged' || !this.#view.acknowledged || !this.#ticket) return;
    const ticket = this.#ticket;
    if (this.#expired(ticket)) {
      this.#expire();
      return;
    }
    const epoch = this.#epoch;
    // Take the authority before the first await. Repeated handlers now observe
    // no ticket and cannot issue a second confirmation POST.
    this.#ticket = null;
    this.#attempt = {replayId: ticket.replayId, parentCaptureId: ticket.captureId, generation: ticket.generation};
    this.#publish({phase: 'submitting', busy: true, acknowledged: false, attempt: {replayId: ticket.replayId, parentCaptureId: ticket.captureId}, message: ''});
    try {
      const reply = await this.api.confirm(ticket.captureId, {schema_version: 1, replay_id: ticket.replayId, confirmation_token: ticket.confirmationToken, confirm_side_effects: true});
      if (epoch !== this.#epoch || !this.#attempt || this.#attempt.replayId !== ticket.replayId) return;
      if (reply.outcome && !this.#matchesAttempt(reply.outcome, this.#attempt)) {
        this.#publish({phase: 'error', busy: false, message: 'Replay outcome did not match this confirmation.'});
        return;
      }
      this.#publish({phase: 'outcome', busy: false, outcome: reply.outcome, message: reply.outcome ? '' : 'Replay submitted. Its outcome is not available yet.'});
    } catch (error) {
      if (this.#handleUnauthorized(error)) return;
      if (epoch !== this.#epoch) return;
      if (this.#rejectConfirmation(error)) return;
      this.#publish({phase: 'error', busy: false, message: 'Confirmation delivery is unknown. It was not retried; check the replay outcome.'});
    }
  }

  #rejectConfirmation(error: unknown): boolean {
    if (!(error instanceof ApiError) || error.kind !== 'http' || error.replayReason === null) return false;
    // A validated replay-error DTO is produced by the confirmation endpoint
    // before execution admission; unlike a transport/parse failure it is a
    // definite rejection of this confirmation attempt.
    this.#attempt = null;
    this.#publish({phase: 'error', busy: false, attempt: null, message: `Replay confirmation was rejected (${error.replayReason}). It was not retried.`});
    return true;
  }

  #matchesAttempt(outcome: ReplayOutcome, attempt: Attempt): boolean {
    return outcome.replay_id === attempt.replayId && outcome.parent_capture_id === attempt.parentCaptureId && outcome.tunnel_generation === attempt.generation;
  }

  async refreshOutcome(): Promise<void> {
    const attempt = this.#attempt;
    if (!attempt || this.#view.busy) return;
    const epoch = this.#epoch;
    this.#safeRead?.abort();
    const controller = new AbortController();
    this.#safeRead = controller;
    this.#publish({busy: true, message: ''});
    try {
      const reply = await this.api.outcome(attempt.replayId, controller.signal);
      if (epoch !== this.#epoch || this.#attempt !== attempt) return;
      if (reply.outcome && !this.#matchesAttempt(reply.outcome, attempt)) {
        this.#publish({phase: 'error', busy: false, message: 'Replay outcome did not match this confirmation.'});
        return;
      }
      this.#publish({phase: 'outcome', busy: false, outcome: reply.outcome, message: reply.outcome ? '' : 'Replay outcome is not available yet.'});
    } catch (error) {
      if (this.#handleUnauthorized(error)) return;
      if (epoch !== this.#epoch || (error instanceof ApiError && error.kind === 'aborted')) return;
      this.#publish({phase: 'error', busy: false, message: this.#message(error)});
    } finally {
      if (this.#safeRead === controller) this.#safeRead = null;
    }
  }
}

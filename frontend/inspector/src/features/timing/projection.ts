import type { Interval, Timing } from '../../api/generated/detail';

export const phases = ['metadata', 'request_head', 'resolve', 'connect', 'request_write', 'response_read', 'response_write', 'upgraded'] as const;
export interface Span { phase: typeof phases[number]; interval: Interval; x: number | null; width: number | null; durationUs: number | null }
export interface Projection { asOfUs: number; terminal: boolean; spans: Span[] }

function offset(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 0;
}
function validInterval(interval: Interval, asOf: number, terminal: boolean): boolean {
  const start = interval.start_offset_us;
  const end = interval.end_offset_us;
  switch (interval.outcome) {
    case 'not_started': case 'skipped': case 'unavailable':
      return start === null && end === null;
    case 'active':
      return !terminal && start !== null && offset(start) && start <= asOf && end === null;
    case 'completed': case 'failed': case 'cancelled':
      return start !== null && end !== null && offset(start) && offset(end) && start <= end && end <= asOf;
    default: return false;
  }
}
export function validTiming(timing: Timing): boolean {
  if (timing.schema_version !== 1 || timing.scope !== 'client_backend') return false;
  if (timing.availability === 'unavailable') return timing.reason !== null && timing.snapshot === null;
  const snapshot = timing.snapshot;
  if (timing.availability !== 'available' || timing.reason !== null || snapshot === null) return false;
  const asOf = snapshot.as_of_offset_us;
  if (!offset(asOf) || !phases.every(phase => validInterval(snapshot.intervals[phase], asOf, snapshot.terminal))) return false;
  const points = [snapshot.backend_first_byte_offset_us, snapshot.backend_final_headers_offset_us,
    snapshot.request_framing_end_offset_us, snapshot.response_framing_end_offset_us];
  if (!points.every(point => point === null || (offset(point) && point <= asOf))) return false;
  if (snapshot.request_framing_valid && snapshot.request_framing_end_offset_us === null) return false;
  if (snapshot.response_framing_valid && snapshot.response_framing_end_offset_us === null) return false;
  return snapshot.backend_final_headers_offset_us === null ||
    (snapshot.backend_first_byte_offset_us !== null && snapshot.backend_first_byte_offset_us <= snapshot.backend_final_headers_offset_us);
}
export function projectTiming(timing: Timing): Projection | null {
  if (!validTiming(timing) || timing.snapshot === null) return null;
  const snapshot = timing.snapshot;
  const scale = Math.max(1, snapshot.as_of_offset_us);
  return {
    asOfUs: snapshot.as_of_offset_us,
    terminal: snapshot.terminal,
    spans: phases.map(phase => {
      const interval = snapshot.intervals[phase];
      const start = interval.start_offset_us;
      const end = interval.end_offset_us ?? (interval.outcome === 'active' ? snapshot.as_of_offset_us : null);
      const durationUs = start === null || end === null ? null : end - start;
      return { phase, interval, durationUs, x: start === null ? null : start / scale * 1000,
        width: durationUs === null ? null : durationUs / scale * 1000 };
    }),
  };
}

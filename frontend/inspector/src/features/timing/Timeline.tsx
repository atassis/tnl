import type {Timing} from '../../api/generated/detail';
import {projectTiming, validTiming} from './projection';

const labels = {
  metadata: 'Metadata decode', request_head: 'Request head probe', resolve: 'Resolve backend', connect: 'TCP connect',
  request_write: 'Local backend writes', response_read: 'Backend read activity', response_write: 'Local tunnel writes', upgraded: 'Upgraded lifetime',
};
const ms = (microseconds: number) => `${(microseconds / 1000).toFixed(3)} ms`;
export function Timeline({timing}: {timing: Timing}) {
  const projected = projectTiming(timing);
  if (!projected) return <section className="timeline"><h3>Timing</h3><p>Timing unavailable: {validTiming(timing) ? timing.reason?.replaceAll('_', ' ') : 'invalid measurement'}.</p></section>;
  const snapshot = timing.snapshot;
  return <section className="timeline" aria-label="Timing">
    <h3>Client ↔ backend</h3>
    <p className="muted">Local forwarder clock, not distributed latency. Intervals overlap; do not sum them. Accepted writes do not prove backend or browser delivery.</p>
    <p>{projected.terminal ? 'Final snapshot' : 'Active at last snapshot'} · {ms(projected.asOfUs)} from forwarder entry</p>
    <svg role="img" aria-label="Client-to-backend timing" viewBox="-5 0 1010 200" className="timeline-chart">
      <title>Overlapping local IO intervals</title>
      {projected.spans.map((span, index) => span.x === null || span.width === null ? null :
        <g key={span.phase}>
          {span.width === 0 ? <circle cx={span.x} cy={index * 24 + 12} r={3} className={`span-${span.interval.outcome}`}/> :
            <rect x={span.x} y={index * 24 + 5} width={span.width} height={14} rx={3} className={`span-${span.interval.outcome}`}/>}
        </g>)}
    </svg>
    <table aria-label="Timing intervals" className="timing-table">
      <thead><tr><th scope="col">Activity</th><th scope="col">State</th><th scope="col">Start offset</th><th scope="col">Observed duration</th></tr></thead>
      <tbody>{projected.spans.map(span => <tr key={span.phase}>
        <th scope="row">{labels[span.phase]}</th><td>{span.interval.outcome.replaceAll('_', ' ')}</td>
        <td>{span.interval.start_offset_us === null ? '—' : `${ms(span.interval.start_offset_us)} from entry`}</td>
        <td>{span.durationUs === null ? '—' : ms(span.durationUs)}</td>
      </tr>)}</tbody>
    </table>
    {snapshot && <dl className="milestones">
      <dt>First backend byte · from entry</dt><dd>{snapshot.backend_first_byte_offset_us === null ? 'Unavailable' : ms(snapshot.backend_first_byte_offset_us)}</dd>
      <dt>Final backend headers · from entry</dt><dd>{snapshot.backend_final_headers_offset_us === null ? 'Unavailable' : ms(snapshot.backend_final_headers_offset_us)}</dd>
      <dt>Request framing</dt><dd>{snapshot.request_framing_end_offset_us === null ? 'Not observed' : `${ms(snapshot.request_framing_end_offset_us)} · ${snapshot.request_framing_valid ? 'valid at snapshot' : 'revoked'}`}</dd>
      <dt>Response framing</dt><dd>{snapshot.response_framing_end_offset_us === null ? 'Not observed' : `${ms(snapshot.response_framing_end_offset_us)} · ${snapshot.response_framing_valid ? 'valid at snapshot' : 'revoked'}`}</dd>
    </dl>}
  </section>;
}

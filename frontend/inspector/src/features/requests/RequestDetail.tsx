import type {DetailReply} from '../../api/generated/detail';
import {MessagePane} from '../../components/MessagePane';
import {Timeline} from '../timing/Timeline';
import {ReplayPanel} from '../replay/ReplayPanel';
import type {ReplayController} from '../replay/controller';
export const detailTabs = ['Request', 'Response', 'Replay', 'Metadata'] as const;
export type DetailTab = typeof detailTabs[number];
export function RequestDetail({record, tab, setTab, replayController}: {record: DetailReply; tab: DetailTab; setTab: (tab: DetailTab) => void; replayController: ReplayController}) {
  const metadata = [
    ['Capture ID', record.capture_id], ['Request ID', record.request_id ?? 'Unavailable (legacy stream)'],
    ['Tunnel ID', record.tunnel_id ?? 'Unavailable'], ['Generation', record.generation], ['Origin', record.origin],
    ['Connection', record.connection], ['Response source', record.response_source], ['Request boundary', record.request_boundary],
    ['Configured backend', record.display_target || 'Unavailable'], ['Resolved address', record.resolved_addr ?? 'Unavailable'],
    ['Local bytes accepted by backend socket', String(record.bytes_to_backend)], ['Local bytes accepted by tunnel', String(record.bytes_to_client)],
    ['Byte semantics', record.byte_count_semantics], ['Failure', record.failure_kind ?? 'None observed'],
    ['Observation incomplete', String(record.observation_incomplete)], ['Omissions', record.omission_reasons.join(', ') || 'None'],
  ];
  return <>
    <header className="detail-heading">
      <p className="muted">External capture · {record.connection} · {record.response_source}</p>
      <h2><span className="method">{record.method}</span> {record.request_target}</h2>
      <p className="canonical">Capture <code data-testid="capture-id">{record.capture_id}</code></p>
      <p className="canonical">Request <code data-testid="request-id">{record.request_id ?? 'Unavailable (legacy stream)'}</code></p>
      <div className="metrics"><div><span>Status</span><strong>{record.status ?? 'Unavailable'}</strong></div><div><span>Task age</span><strong>{record.duration_ms} ms</strong></div><div><span>State</span><strong>{record.state}</strong></div><div><span>Backend bytes accepted</span><strong>{record.bytes_to_backend}</strong></div></div>
    </header>
    <div className="detail-content"><Timeline timing={record.timing}/>
      <div role="tablist" aria-label="Capture details" className="tabs">
        {detailTabs.map((name, index) => <button type="button" role="tab" key={name} id={`detail-tab-${name}`} aria-controls="detail-panel" aria-selected={tab === name} tabIndex={tab === name ? 0 : -1} onClick={() => setTab(name)} onKeyDown={event => {
          const next = event.key === 'ArrowRight' ? detailTabs[(index + 1) % detailTabs.length] : event.key === 'ArrowLeft' ? detailTabs[(index + detailTabs.length - 1) % detailTabs.length] : event.key === 'Home' ? detailTabs[0] : event.key === 'End' ? detailTabs[detailTabs.length - 1] : undefined;
          if (next) {event.preventDefault();setTab(next);document.getElementById(`detail-tab-${next}`)?.focus();}
        }}>{name}</button>)}
      </div>
      <div role="tabpanel" id="detail-panel" aria-labelledby={`detail-tab-${tab}`}>
        {tab === 'Request' && <MessagePane title="Request" message={record.request}/>}
        {tab === 'Response' && <MessagePane title="Response" message={record.response}/>}
        {tab === 'Metadata' && <dl className="metadata">{metadata.map(([key, value]) => <div key={key}><dt>{key}</dt><dd>{value}</dd></div>)}</dl>}
        {tab === 'Replay' && <ReplayPanel controller={replayController} retainedOutcome={record.last_replay_outcome}/>}
      </div>
    </div>
  </>;
}

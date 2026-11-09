import type {ListReply, Summary} from '../../api/generated/list';
export function RequestList({page, selectedId, select}: {page: ListReply | null; selectedId: string | null; select: (row: Summary) => void}) {
  return <div className="request-list" aria-label="Captured requests">
    {page === null ? <p className="empty">Waiting for history.</p> : page.items.length === 0 ? <p className="empty">No matching captures retained.</p> :
      page.items.map(row => <button type="button" className={`request ${row.capture_id === selectedId ? 'selected' : ''}`} key={row.capture_id}
        aria-label={`Inspect ${row.method} ${row.request_target}`} aria-pressed={row.capture_id === selectedId} data-capture-id={row.capture_id} onClick={() => select(row)}>
        <span className="request-top"><span className="method">{row.method}</span><span className="request-path">{row.request_target}</span>
          <span className={row.status !== null && row.status >= 400 ? 'failure' : 'status-code'}>{row.status ?? '—'}</span></span>
        <span className="request-meta"><span>{row.state} · {row.connection}</span><span>{row.duration_ms} ms task age</span></span>
      </button>)}
  </div>;
}

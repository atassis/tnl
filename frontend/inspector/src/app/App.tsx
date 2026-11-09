import {useCallback, useState} from 'react';
import type {InspectorClient, ListOptions} from '../api/client';
import {useTheme} from './useTheme';
import {parseTheme} from './theme';
import {useHistory} from '../features/requests/useHistory';
import {RequestList} from '../features/requests/RequestList';
import {RequestDetail} from '../features/requests/RequestDetail';
import type {DetailTab} from '../features/requests/RequestDetail';
import {ReplayController} from '../features/replay/controller';
import type {Summary} from '../api/generated/list';
export type InspectorApi = Pick<InspectorClient, 'stats' | 'list' | 'detail' | 'availability' | 'prepare' | 'confirm' | 'outcome'>;
export function App({api, synthetic = false}: {api: InspectorApi; synthetic?: boolean}) {
  const theme = useTheme();
  const [paused, setPaused] = useState(false);
  const [options, setOptions] = useState<ListOptions>({});
  const [q, setQ] = useState('');
  const [method, setMethod] = useState('');
  const [status, setStatus] = useState('');
  const [state, setState] = useState('');
  const [formError, setFormError] = useState('');
  const [tab, setTab] = useState<DetailTab>('Request');
  // Construction is inert: replay reads and mutations require an explicit user action.
  const [replay] = useState(() => new ReplayController(api));
  const onSelect = useCallback((row: Summary) => {replay.select(row); setTab('Request');}, [replay]);
  const invalidate = useCallback(() => replay.invalidate(), [replay]);
  const history = useHistory(api, options, paused, invalidate, onSelect);
  const latest = () => { const next = {...options}; delete next.before; setOptions(next); };
  return <main className="app">
    <header className="app-header"><div className="brand"><div className="logo" aria-hidden="true">t_</div><div><h1>tnl <span>/</span> inspect</h1><p>{history.detail ? `${history.detail.tunnel} → ${history.detail.display_target}` : 'Client-side inspector'}</p></div></div>
      <div className="top-controls">{synthetic && <span className="synthetic">Synthetic development data</span>}
        <span className="connection">{history.forbidden ? 'Access expired' : history.stats === null ? 'Waiting for inspector' : history.stats.tunnel.connected ? 'Connected' : 'Disconnected'}</span>
        <label className="theme-control">Theme <select aria-label="Theme" value={theme.mode} onChange={event => theme.setMode(parseTheme(event.target.value))}><option value="auto">Auto · system</option><option value="light">Light</option><option value="dark">Dark</option></select></label>
      </div>
    </header>
    <div className="workspace"><aside className="list-pane" aria-label="Request history"><div className="list-heading"><h2>Requests <span>{history.stats?.records ?? '—'}</span></h2><button type="button" onClick={() => setPaused(value => !value)} aria-pressed={paused}>{paused ? 'Resume updates' : 'Pause updates'}</button></div>
      <form className="filters" onSubmit={event => {
        event.preventDefault();
        if (new TextEncoder().encode(q).length > 256 || (method && !/^[A-Za-z]{1,32}$/.test(method)) || (status && !/^[1-5][0-9]{2}$/.test(status))) {setFormError('Use at most 256 UTF-8 search bytes, an alphabetic method, and an exact status from 100–599.');return;}
        setFormError('');setOptions({...q ? {q} : {}, ...method ? {method} : {}, ...status ? {status} : {}, ...state ? {state} : {}});
      }}>
        <label>Search<input aria-label="Search" value={q} maxLength={256} placeholder="Method, path or capture ID" onChange={event => setQ(event.target.value)}/></label>
        <div className="filter-pair"><label>Method<input value={method} maxLength={32} placeholder="Any" onChange={event => setMethod(event.target.value.toUpperCase())}/></label><label>Status<input value={status} maxLength={3} inputMode="numeric" placeholder="Any" onChange={event => setStatus(event.target.value)}/></label></div>
        <label>State<select value={state} onChange={event => setState(event.target.value)}><option value="">Any state</option>{['active', 'complete', 'failed', 'incomplete', 'upgraded'].map(value => <option key={value} value={value}>{value}</option>)}</select></label>
        <button type="submit" disabled={paused || history.forbidden}>Apply filters</button>{formError && <p role="alert">{formError}</p>}
      </form>
      <p className="list-caption">Newest first · {paused ? 'updates paused; capture continues' : 'bounded live updates'}</p>
      <RequestList page={history.page} selectedId={history.selectedId} select={history.select}/>
      <footer className="list-footer"><button type="button" onClick={latest} disabled={paused || history.forbidden || options.before === undefined}>Latest</button><span>Memory only</span><button type="button" disabled={paused || history.forbidden || history.page?.next_before == null} onClick={() => {if (history.page?.next_before != null) setOptions({...options, before: history.page.next_before});}}>Older requests</button></footer>
    </aside>
      <section className="detail-pane" aria-label="Selected capture">
        {history.message && <p className="notice" role="status">{history.message}</p>}
        {history.detail ? <RequestDetail record={history.detail} tab={tab} setTab={setTab} replayController={replay}/> : <div className="empty"><h2>{history.detailState === 'loading' ? 'Loading capture…' : history.detailState === 'evicted' ? 'Capture evicted' : history.detailState === 'busy' ? 'Inspector busy' : 'Select a captured request'}</h2><p>Sanitized request and response details stay in this process. Reopen the private startup link after restarting the client.</p></div>}
      </section>
    </div>
    <footer className="app-footer"><span>Loopback · memory-only history · no WebSocket frame capture</span><span>Sanitized preview is not the executable request.</span></footer>
  </main>;
}

import {useSyncExternalStore} from 'react';
import type {ReplayOutcome} from '../../api/generated/outcome';
import type {ReplayController} from './controller';

export function ReplayPanel({controller, retainedOutcome = null}: {controller: ReplayController; retainedOutcome?: ReplayOutcome | null}) {
  const view = useSyncExternalStore(controller.subscribe, controller.getSnapshot, controller.getSnapshot);
  const canPrepare = view.phase === 'idle' && !view.busy && view.availability?.state === 'eligible';
  const canConfirm = view.phase === 'reviewing_acknowledged' && !view.busy;
  const canReadOutcome = view.attempt !== null && !view.busy && (view.phase === 'outcome' || view.phase === 'error');
  return <section className="replay-panel" aria-labelledby="replay-heading">
    <h3 id="replay-heading">Replay</h3>
    <p>Availability: <strong>{view.availability?.state ?? 'not checked'}</strong>{view.availability?.reason ? ` · ${view.availability.reason}` : ''}</p>
    <p>Raw retention and execution are separate from sanitized preview.</p>
    {(view.phase === 'idle' || (view.phase === 'error' && view.canRecoverReview && view.attempt === null)) && <div className="replay-actions">
      <button type="button" onClick={() => {void controller.checkAvailability();}} disabled={view.busy}>Check replay availability</button>
      <button type="button" onClick={() => {void controller.prepare();}} disabled={!canPrepare}>Prepare replay review</button>
    </div>}
    {view.phase === 'preparing' && <p role="status">Preparing replay review. This does not execute the backend request.</p>}
    {(view.phase === 'reviewing' || view.phase === 'reviewing_acknowledged') && view.review && <div className="replay-review">
      <h4>Replay review</h4>
      <dl className="metadata"><div><dt>Method</dt><dd>{view.review.method}</dd></div><div><dt>Request target</dt><dd>{view.review.path}</dd></div><div><dt>Backend target</dt><dd>{view.review.target}</dd></div><div><dt>Host policy</dt><dd>{view.review.hostPolicy}</dd></div><div><dt>Retained request bytes</dt><dd>{view.review.requestBytes}</dd></div></dl>
      <p className="failure">This may repeat side effects and send retained request secrets to the original permitted backend.</p>
      <ul>{view.review.warnings.map(warning => <li key={warning}>{warning}</li>)}</ul>
      <label className="replay-ack"><input type="checkbox" checked={view.acknowledged} onChange={event => controller.acknowledge(event.target.checked)}/> I understand this can cause side effects</label>
      <div className="replay-actions"><button type="button" onClick={() => {void controller.confirm();}} disabled={!canConfirm}>Confirm replay</button></div>
    </div>}
    {view.phase === 'submitting' && <p role="status">Submitting one replay confirmation…</p>}
    {view.phase === 'outcome' && <div className="replay-outcome"><h4>Replay outcome</h4>{view.outcome ? <p>{view.outcome.state} · delivery {view.outcome.delivery}{view.outcome.status === null ? '' : ` · status ${view.outcome.status}`}</p> : <p>{view.message}</p>}</div>}
    {retainedOutcome && <div className="replay-outcome"><h4>Last retained replay outcome (read-only)</h4><p>{retainedOutcome.state} · delivery {retainedOutcome.delivery}{retainedOutcome.status === null ? '' : ` · status ${retainedOutcome.status}`}</p><p>This server-retained result does not restore a review, acknowledgment, or confirmation authority.</p></div>}
    {view.message && view.phase === 'error' && <p className="failure" role="alert">{view.message}</p>}
    {canReadOutcome && <div className="replay-actions"><button type="button" onClick={() => {void controller.refreshOutcome();}}>Check replay outcome</button></div>}
  </section>;
}

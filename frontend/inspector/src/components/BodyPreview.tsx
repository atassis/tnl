import type {BodyPreview as BodyData} from '../api/generated/detail';
export function BodyPreview({body}: {body: BodyData}) {
  return <div className="body-preview">
    <p className="muted">{body.observed_payload_bytes} observed payload bytes</p>
    <p>Preview: {body.state}{body.redacted ? ' · redacted' : ''}{body.reason ? ` · ${body.reason}` : ''}</p>
    {body.text !== null ? <pre data-testid="captured-body">{body.text}</pre> : <p className="muted">No retained body preview.</p>}
  </div>;
}

import type {Message} from '../api/generated/detail';
import {BodyPreview} from './BodyPreview';
export function MessagePane({title, message}: {title: string; message: Message}) {
  return <section className="message-pane" aria-label={title}>
    <h3>{title}</h3>
    <h4>Headers · {message.headers_state}</h4>
    {message.headers.length > 0 ? <table className="headers-table" aria-label={`${title} headers`}>
      <tbody>{message.headers.map((header, index) => <tr key={`${index}-${header.name}`}>
        <th scope="row">{header.name}</th><td>{header.value}{header.redacted ? ' · redacted' : ''}</td>
      </tr>)}</tbody>
    </table> : <p className="muted">No retained headers.</p>}
    <h4>Sanitized body</h4><BodyPreview body={message.body}/>
  </section>;
}

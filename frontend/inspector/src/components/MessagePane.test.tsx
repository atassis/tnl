import {afterEach,expect,it} from 'vitest';
import {cleanup,render,screen} from '@testing-library/react';
import {MessagePane} from './MessagePane';
afterEach(cleanup);
it('renders sanitized headers and captured markup only as literal text',()=>{
  const canary='<svg onload="window.XSS_CANARY=1"><script>bad()</script></svg>';
  const {container}=render(<MessagePane title="Request" message={{headers_state:'complete',headers:[{name:'x-fixture',value:canary,redacted:false}],body:{mode:'json',state:'available',retained_bytes:canary.length,redacted:true,text:canary,reason:null,observed_payload_bytes:1024}}}/>);
  expect(screen.getByRole('heading',{name:'Request'})).toBeTruthy();
  expect(container.querySelector('[data-testid="captured-body"]')?.textContent).toBe(canary);
  expect(container.querySelector('svg,script,img,iframe,a,style')).toBeNull();
  expect(screen.getByText('1024 observed payload bytes')).toBeTruthy();
});
it('explains omission instead of inventing an empty body',()=>{
  render(<MessagePane title="Response" message={{headers_state:'capped',headers:[],body:{mode:'json',state:'omitted',retained_bytes:0,redacted:false,text:null,reason:'capture-limit',observed_payload_bytes:4097}}}/>);
  expect(screen.getByText(/capture-limit/)).toBeTruthy();expect(screen.getByText(/capped/)).toBeTruthy();
  expect(screen.queryByTestId('captured-body')).toBeNull();
});

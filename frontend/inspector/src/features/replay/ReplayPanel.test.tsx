import {afterEach,expect,it,vi} from 'vitest';
import {StrictMode} from 'react';
import {act,cleanup,fireEvent,render,screen,waitFor} from '@testing-library/react';
import type {ReplayApi} from './controller';
import {ReplayController} from './controller';
import {ReplayPanel} from './ReplayPanel';
import {ApiError,InspectorClient} from '../../api/client';
import {detailFixture,previewFixture} from '../../fixtures/contracts';
import {RequestDetail} from '../requests/RequestDetail';
import type {Summary} from '../../api/generated/list';

afterEach(cleanup);
const id='00000000000000000000000000';
const token='PANEL_TICKET_CANARY';
const row:Summary={capture_id:id,generation:id,method:'POST',request_target:'/orders',connection:'connected',request_boundary:'effective_backend',response_source:'backend',bytes_from_backend:0,bytes_to_backend:0,duration_ms:1,failure_kind:null,request_id:null,sequence:1,started_at_ms:1,state:'complete',status:200};
const api:ReplayApi={
  availability:vi.fn().mockResolvedValue({schema_version:1,availability:{state:'eligible',reason:null,raw_expires_at_unix_ms:2_000,request_body_bytes:1}}),
  prepare:vi.fn().mockResolvedValue({schema_version:1,method:'POST',request_target:'/orders',preview:{capture_id:id,tunnel_generation:id,replay_id:'00000000000000000000000001',confirmation_token:token,confirmation_expires_at_unix_ms:1_500,raw_expires_at_unix_ms:2_000,request_body_bytes:1,target:'127.0.0.1:3000',host_policy:'original',warnings:['One attempt','Side effects','Original secrets']}}),
  confirm:vi.fn().mockResolvedValue({schema_version:1,outcome:null}),
  outcome:vi.fn().mockResolvedValue({schema_version:1,outcome:null}),
};

it('requires explicit review and acknowledgement without rendering the ticket or mutating on mount',async()=>{
  const controller=new ReplayController(api,()=>1_000);controller.select(row);
  const {rerender}=render(<StrictMode><ReplayPanel controller={controller}/></StrictMode>);
  expect(api.prepare).not.toHaveBeenCalled();expect(api.confirm).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button',{name:'Check replay availability'}));
  await waitFor(()=>expect(screen.getByText('eligible')).toBeTruthy());
  fireEvent.click(screen.getByRole('button',{name:'Prepare replay review'}));
  await screen.findByText('Replay review');
  expect(document.body.textContent).not.toContain(token);
  expect(screen.getByRole('button',{name:'Confirm replay'}).hasAttribute('disabled')).toBe(true);
  fireEvent.click(screen.getByLabelText('I understand this can cause side effects'));
  rerender(<StrictMode><ReplayPanel controller={controller}/></StrictMode>);
  expect((screen.getByLabelText('I understand this can cause side effects') as HTMLInputElement).checked).toBe(true);
  expect(api.confirm).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button',{name:'Confirm replay'}));
  await waitFor(()=>expect(api.confirm).toHaveBeenCalledTimes(1));
  rerender(<StrictMode><ReplayPanel controller={controller}/></StrictMode>);
  expect(api.confirm).toHaveBeenCalledTimes(1);
});

for (const failure of ['availability', 'prepare', 'expiry'] as const) {
  it(`recovers ${failure} only through explicit availability and fresh unacknowledged review`, async () => {
    let now = 1_000;
    const service = {...api, availability:vi.fn(api.availability), prepare:vi.fn(api.prepare), confirm:vi.fn(api.confirm)};
    if (failure === 'availability') service.availability.mockRejectedValueOnce(new ApiError('network'));
    if (failure === 'prepare') service.prepare.mockRejectedValueOnce(new ApiError('busy',503));
    const controller = new ReplayController(service,()=>now); controller.select(row);
    render(<ReplayPanel controller={controller}/>);
    fireEvent.click(screen.getByRole('button',{name:'Check replay availability'}));
    if (failure !== 'availability') {
      await waitFor(()=>expect(screen.getByRole('button',{name:'Prepare replay review'}).hasAttribute('disabled')).toBe(false));
      fireEvent.click(screen.getByRole('button',{name:'Prepare replay review'}));
    }
    if (failure === 'expiry') {
      await screen.findByText('Replay review');
      fireEvent.click(screen.getByLabelText('I understand this can cause side effects'));
      now = 1_500;
      fireEvent.click(screen.getByRole('button',{name:'Confirm replay'}));
    }
    await screen.findByRole('alert');
    const prepares = service.prepare.mock.calls.length;
    expect(service.confirm).not.toHaveBeenCalled();
    now = 1_000;
    fireEvent.click(screen.getByRole('button',{name:'Check replay availability'}));
    await waitFor(()=>expect(screen.getByRole('button',{name:'Prepare replay review'}).hasAttribute('disabled')).toBe(false));
    expect(service.prepare).toHaveBeenCalledTimes(prepares);
    fireEvent.click(screen.getByRole('button',{name:'Prepare replay review'}));
    await screen.findByText('Replay review');
    expect((screen.getByLabelText('I understand this can cause side effects') as HTMLInputElement).checked).toBe(false);
    expect(service.confirm).not.toHaveBeenCalled();
  });
}

it.each(['busy', 'overloaded', 'runtime_unavailable'] as const)('recovers a validated %s preparation refusal only after explicit user actions',async reason=>{
  const preview=previewFixture();
  let preparations=0;
  const fetcher=vi.fn<typeof fetch>().mockImplementation(async path=>{
    const preparing=String(path).endsWith('/prepare');
    if(preparing && ++preparations===1)return new Response(JSON.stringify({schema_version:1,error:reason}),{status:503,headers:{'Content-Type':'application/json'}});
    return new Response(JSON.stringify(preparing?preview:{schema_version:1,availability:{state:'eligible',reason:null,raw_expires_at_unix_ms:preview.preview.raw_expires_at_unix_ms,request_body_bytes:preview.preview.request_body_bytes}}),{headers:{'Content-Type':'application/json'}});
  });
  const controller=new ReplayController(new InspectorClient('a'.repeat(64),fetcher),()=>0);
  controller.select({...row,capture_id:preview.preview.capture_id,generation:preview.preview.tunnel_generation});
  render(<ReplayPanel controller={controller}/>);
  fireEvent.click(screen.getByRole('button',{name:'Check replay availability'}));
  await waitFor(()=>expect(screen.getByRole('button',{name:'Prepare replay review'}).hasAttribute('disabled')).toBe(false));
  fireEvent.click(screen.getByRole('button',{name:'Prepare replay review'}));
  await screen.findByRole('alert');
  expect(preparations).toBe(1);
  fireEvent.click(screen.getByRole('button',{name:'Check replay availability'}));
  await waitFor(()=>expect(screen.getByRole('button',{name:'Prepare replay review'}).hasAttribute('disabled')).toBe(false));
  expect(preparations).toBe(1);
  fireEvent.click(screen.getByRole('button',{name:'Prepare replay review'}));
  await screen.findByText('Replay review');
  expect(preparations).toBe(2);
  expect((screen.getByLabelText('I understand this can cause side effects') as HTMLInputElement).checked).toBe(false);
  expect(fetcher.mock.calls.some(([path])=>String(path).endsWith('/confirm'))).toBe(false);
});

it('keeps unknown confirmation delivery outcome-only across remount',async()=>{
  const service = {...api, confirm:vi.fn().mockRejectedValue(new ApiError('network')), outcome:vi.fn(api.outcome)};
  const controller = new ReplayController(service,()=>1_000);controller.select(row);
  await controller.checkAvailability();await controller.prepare();controller.acknowledge(true);await controller.confirm();
  const first=render(<ReplayPanel controller={controller}/>);first.unmount();render(<ReplayPanel controller={controller}/>);
  expect(screen.queryByRole('button',{name:'Check replay availability'})).toBeNull();
  expect(screen.queryByRole('button',{name:'Prepare replay review'})).toBeNull();
  fireEvent.click(screen.getByRole('button',{name:'Check replay outcome'}));
  await waitFor(()=>expect(service.outcome).toHaveBeenCalledTimes(1));
  expect(service.confirm).toHaveBeenCalledTimes(1);
});

it('shows retained server outcome on returning selection and reload without restoring authority',async()=>{
  const controller = new ReplayController(api,()=>1_000);controller.select(row);
  const record = detailFixture();
  record.last_replay_outcome = {replay_id:'00000000000000000000000001',parent_capture_id:record.capture_id,tunnel_generation:record.generation,state:'complete',delivery:'response_received',status:201,duration_ms:2,error_code:null,request_body_bytes:1,response_body_bytes:0,response_complete:true};
  const first=render(<RequestDetail record={record} tab="Replay" setTab={()=>{}} replayController={controller}/>);
  await act(async()=>{controller.select({...row,capture_id:'00000000000000000000000002'});controller.select(row);});
  expect(screen.getByRole('heading',{name:'Last retained replay outcome (read-only)'})).toBeTruthy();
  expect(screen.getByText(/response_received.*201/)).toBeTruthy();
  first.unmount();
  const reloaded=new ReplayController(api,()=>1_000);reloaded.select(row);
  render(<RequestDetail record={record} tab="Replay" setTab={()=>{}} replayController={reloaded}/>);
  expect(screen.getByRole('heading',{name:'Last retained replay outcome (read-only)'})).toBeTruthy();
  expect(screen.queryByRole('button',{name:'Confirm replay'})).toBeNull();
  expect(screen.queryByRole('checkbox')).toBeNull();
  expect(reloaded.getSnapshot().attempt).toBeNull();
});

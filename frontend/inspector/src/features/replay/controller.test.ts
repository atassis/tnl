import {expect,it,vi} from 'vitest';
import type {ReplayAvailability} from '../../api/generated/availability';
import type {OutcomeReply,ReplayOutcome} from '../../api/generated/outcome';
import type {PreviewReply} from '../../api/generated/preview';
import type {Summary} from '../../api/generated/list';
import {ApiError,InspectorClient} from '../../api/client';
import {ReplayController,type ReplayApi} from './controller';

const captureId='00000000000000000000000000';
const otherCaptureId='00000000000000000000000001';
const generation='00000000000000000000000000';
const replayId='00000000000000000000000002';
const confirmationToken='TICKET_MUST_NEVER_REACH_A_VIEW';
const row=(id=captureId):Summary=>({capture_id:id,generation,method:'POST',request_target:'/orders',connection:'connected',request_boundary:'effective_backend',response_source:'backend',bytes_from_backend:0,bytes_to_backend:0,duration_ms:1,failure_kind:null,request_id:null,sequence:1,started_at_ms:1,state:'complete',status:200});
const eligible:ReplayAvailability={state:'eligible',reason:null,raw_expires_at_unix_ms:2_000,request_body_bytes:12};
const preview=(id=captureId):PreviewReply=>({schema_version:1,method:'POST',request_target:'/orders',preview:{capture_id:id,tunnel_generation:generation,replay_id:replayId,confirmation_token:confirmationToken,confirmation_expires_at_unix_ms:1_500,raw_expires_at_unix_ms:2_000,request_body_bytes:12,target:'127.0.0.1:3000',host_policy:'original',warnings:['One attempt','Side effects','Original secrets']}});
const outcome:ReplayOutcome={replay_id:replayId,parent_capture_id:captureId,tunnel_generation:generation,delivery:'response_received',duration_ms:1,error_code:null,request_body_bytes:12,response_body_bytes:4,response_complete:true,state:'complete',status:200};
function api(overrides:Partial<ReplayApi>={}):ReplayApi {return {availability:vi.fn().mockResolvedValue({schema_version:1,availability:eligible}),prepare:vi.fn().mockResolvedValue(preview()),confirm:vi.fn().mockResolvedValue({schema_version:1,outcome}),outcome:vi.fn().mockResolvedValue({schema_version:1,outcome}),...overrides};}

it('ignores a late preparation after selection changes and keeps the ticket out of views',async()=>{
  let resolve:(value:PreviewReply)=>void=()=>{};
  const pending=new Promise<PreviewReply>(value=>{resolve=value;});
  const service=api({prepare:vi.fn().mockReturnValue(pending)});
  const controller=new ReplayController(service,()=>1_000);
  controller.select(row());
  await controller.checkAvailability();
  const prepare=controller.prepare();
  expect(controller.getSnapshot().phase).toBe('preparing');
  controller.select(row(otherCaptureId));
  resolve(preview());
  await prepare;
  const view=controller.getSnapshot();
  expect(view.phase).toBe('idle');
  expect(JSON.stringify(view)).not.toContain(confirmationToken);
  expect(service.confirm).not.toHaveBeenCalled();
});

it('requires a fresh acknowledgement and consumes the ticket before a same-turn double confirm',async()=>{
  let resolve:(value:OutcomeReply)=>void=()=>{};
  const pending=new Promise<OutcomeReply>(value=>{resolve=value;});
  const service=api({confirm:vi.fn().mockReturnValue(pending)});
  const controller=new ReplayController(service,()=>1_000);
  controller.select(row());
  await controller.checkAvailability();
  await controller.prepare();
  expect(controller.getSnapshot().phase).toBe('reviewing');
  await controller.confirm();
  expect(service.confirm).not.toHaveBeenCalled();
  controller.acknowledge(true);
  expect(controller.getSnapshot().phase).toBe('reviewing_acknowledged');
  const first=controller.confirm();
  const second=controller.confirm();
  expect(service.confirm).toHaveBeenCalledTimes(1);
  expect(service.confirm).toHaveBeenCalledWith(captureId, expect.objectContaining({replay_id: replayId,confirm_side_effects:true}));
  expect(controller.getSnapshot().phase).toBe('submitting');
  resolve({schema_version:1,outcome});
  await Promise.all([first,second]);
  expect(controller.getSnapshot().outcome).toEqual(outcome);
});

it('clears authority on invalidation, expires acknowledgements, and never retries an unknown confirmation',async()=>{
  let now=1_000;
  const service=api({confirm:vi.fn().mockRejectedValue(new ApiError('network'))});
  const controller=new ReplayController(service,()=>now);
  controller.select(row());
  await controller.checkAvailability();
  await controller.prepare();
  controller.acknowledge(true);
  controller.invalidate();
  expect(controller.getSnapshot().phase).toBe('idle');
  expect(JSON.stringify(controller.getSnapshot())).not.toContain(confirmationToken);
  await controller.confirm();
  expect(service.confirm).not.toHaveBeenCalled();
  await controller.checkAvailability();
  await controller.prepare();
  controller.acknowledge(true);
  now=1_500;
  await controller.confirm();
  expect(service.confirm).not.toHaveBeenCalled();
  expect(controller.getSnapshot().phase).toBe('error');

  now=1_000;
  await controller.checkAvailability();
  await controller.prepare();
  controller.acknowledge(true);
  await controller.confirm();
  expect(service.confirm).toHaveBeenCalledTimes(1);
  expect(controller.getSnapshot().message).toMatch(/unknown/i);
  await controller.confirm();
  expect(service.confirm).toHaveBeenCalledTimes(1);
  await controller.refreshOutcome();
  expect(service.outcome).toHaveBeenCalledTimes(1);
  expect(service.confirm).toHaveBeenCalledTimes(1);
});

it('reports a validated inspector rejection as definite and keeps network delivery unknown',async()=>{
  const rejected=api({confirm:vi.fn().mockRejectedValue(new ApiError('http',400,'invalid_confirmation'))});
  const controller=new ReplayController(rejected,()=>1_000);
  controller.select(row());await controller.checkAvailability();await controller.prepare();controller.acknowledge(true);
  await controller.confirm();
  expect(controller.getSnapshot().phase).toBe('error');
  expect(controller.getSnapshot().message).toMatch(/rejected/i);
  expect(controller.getSnapshot().message).not.toMatch(/unknown/i);
  expect(controller.getSnapshot().attempt).toBeNull();
  await controller.refreshOutcome();
  expect(rejected.outcome).not.toHaveBeenCalled();

  const unknown=api({confirm:vi.fn().mockRejectedValue(new ApiError('network'))});
  const uncertain=new ReplayController(unknown,()=>1_000);
  uncertain.select(row());await uncertain.checkAvailability();await uncertain.prepare();uncertain.acknowledge(true);
  await uncertain.confirm();
  expect(uncertain.getSnapshot().message).toMatch(/unknown/i);
  expect(uncertain.getSnapshot().attempt).not.toBeNull();
});

it('clears review authority when a safe read reports 401',async()=>{
  const service=api({availability:vi.fn().mockRejectedValue(new ApiError('unauthorized',401))});
  const controller=new ReplayController(service,()=>1_000);
  controller.select(row());
  await controller.checkAvailability();
  expect(controller.getSnapshot().phase).toBe('error');
  expect(controller.getSnapshot().review).toBeNull();
  expect(JSON.stringify(controller.getSnapshot())).not.toContain(confirmationToken);
});

it('binds outcome reads to the selected capture and aborts superseded safe reads',async()=>{
  let reject:(reason:unknown)=>void=()=>{};
  const pending=new Promise<OutcomeReply>((_resolve,reason)=>{reject=reason;});
  const service=api({outcome:vi.fn().mockReturnValue(pending)});
  const controller=new ReplayController(service,()=>1_000);
  controller.select(row());
  await controller.checkAvailability();
  await controller.prepare();
  controller.acknowledge(true);
  await controller.confirm();
  const refresh=controller.refreshOutcome();
  controller.select(row(otherCaptureId));
  reject(new ApiError('aborted'));
  await refresh;
  expect(controller.getSnapshot().phase).toBe('idle');
  expect(service.outcome).toHaveBeenCalledTimes(1);
});

it('rejects the first A preparation after A to B to A even when identity matches again',async()=>{
  let resolve:(value:PreviewReply)=>void=()=>{};
  const service=api({prepare:vi.fn().mockReturnValue(new Promise<PreviewReply>(yes=>{resolve=yes;}))});
  const controller=new ReplayController(service,()=>1_000);
  controller.select(row());await controller.checkAvailability();const pending=controller.prepare();
  controller.select(row(otherCaptureId));controller.select(row());
  resolve(preview());await pending;
  expect(controller.getSnapshot().phase).toBe('idle');expect(controller.getSnapshot().review).toBeNull();
  controller.acknowledge(true);await controller.confirm();expect(service.confirm).not.toHaveBeenCalled();
});
it('treats a late superseded preparation 401 as globally revoking',async()=>{
  let reject:(reason:unknown)=>void=()=>{};
  const service=api({prepare:vi.fn().mockReturnValueOnce(new Promise<PreviewReply>((_yes,no)=>{reject=no;})).mockResolvedValue(preview(otherCaptureId))});
  const controller=new ReplayController(service,()=>1_000);
  controller.select(row());await controller.checkAvailability();const pending=controller.prepare();
  controller.select(row(otherCaptureId));await controller.checkAvailability();await controller.prepare();controller.acknowledge(true);
  expect(controller.getSnapshot().phase).toBe('reviewing_acknowledged');
  reject(new ApiError('unauthorized',401));await pending;
  expect(controller.getSnapshot().phase).toBe('error');expect(controller.getSnapshot().review).toBeNull();
  await controller.confirm();expect(service.confirm).not.toHaveBeenCalled();
});

it.each([[404,'not_retained'],[503,'busy']] as const)('reports fetch-backed confirmation %i replay refusal as definite with no outcome authority',async(status,reason)=>{
  const fetcher=vi.fn<typeof fetch>().mockResolvedValue(new Response(JSON.stringify({schema_version:1,error:reason}),{status,headers:{'Content-Type':'application/json'}}));
  const client=new InspectorClient('a'.repeat(64),fetcher);
  const service=api({confirm:(id,body)=>client.confirm(id,body)});
  const controller=new ReplayController(service,()=>1_000);
  controller.select(row());await controller.checkAvailability();await controller.prepare();controller.acknowledge(true);
  await controller.confirm();
  expect(controller.getSnapshot().message).toContain(`rejected (${reason})`);
  expect(controller.getSnapshot().attempt).toBeNull();
  await controller.confirm();await controller.refreshOutcome();
  expect(fetcher).toHaveBeenCalledTimes(1);expect(service.outcome).not.toHaveBeenCalled();
});
it.each([404,503])('keeps malformed and non-JSON fetch-backed mutation %i delivery unknown',async status=>{
  for(const [body,contentType]of [['{}','application/json'],['not JSON','text/html']]){
    const client=new InspectorClient('a'.repeat(64),vi.fn<typeof fetch>().mockResolvedValue(new Response(body,{status,headers:{'Content-Type':contentType!}})));
    const service=api({confirm:(id,request)=>client.confirm(id,request)});
    const controller=new ReplayController(service,()=>1_000);
    controller.select(row());await controller.checkAvailability();await controller.prepare();controller.acknowledge(true);await controller.confirm();
    expect(controller.getSnapshot().message).toMatch(/unknown/i);expect(controller.getSnapshot().attempt).not.toBeNull();
    await controller.refreshOutcome();expect(service.outcome).toHaveBeenCalledTimes(1);
  }
});

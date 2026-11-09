import {afterEach,expect,it,vi} from 'vitest';
import {act,cleanup,renderHook,waitFor} from '@testing-library/react';
import {ApiError} from '../../api/client';
import {detailFixture,listFixture,previewFixture,statsFixture} from '../../fixtures/contracts';
import {useHistory} from './useHistory';
import type {HistoryApi} from './useHistory';
import type {DetailReply} from '../../api/generated/detail';
import type {OutcomeReply} from '../../api/generated/outcome';
import {ReplayController,type ReplayApi} from '../replay/controller';

afterEach(cleanup);
function api():HistoryApi {return {stats:vi.fn().mockResolvedValue(statsFixture()),list:vi.fn().mockResolvedValue(listFixture()),detail:vi.fn().mockResolvedValue(detailFixture())};}
function replayApi(confirm: ReplayApi['confirm']):ReplayApi {return {
  availability:vi.fn().mockResolvedValue({schema_version:1,availability:{state:'eligible',reason:null,raw_expires_at_unix_ms:2_000,request_body_bytes:0}}),
  prepare:vi.fn().mockResolvedValue(previewFixture()),confirm,outcome:vi.fn().mockResolvedValue({schema_version:1,outcome:null}),
};}

it('ignores a late detail result and invalidates selection synchronously',async()=>{
  const service=api();const first=detailFixture();const second=detailFixture();second.capture_id='00000000000000000000000002';
  const pending: {resolve:(value:DetailReply)=>void}={resolve:()=>{}};
  const firstPromise=new Promise<DetailReply>(resolve=>{pending.resolve=resolve;});
  service.detail=vi.fn().mockReturnValueOnce(firstPromise).mockResolvedValueOnce(second);
  const onSelect=vi.fn();const hook=renderHook(()=>useHistory(service,{},true,()=>{},onSelect));
  const summary=listFixture().items[0];if(!summary)throw Error('fixture');
  act(()=>hook.result.current.select(summary));expect(onSelect).toHaveBeenCalledTimes(1);
  act(()=>hook.result.current.select({...summary,capture_id:second.capture_id}));
  await waitFor(()=>expect(hook.result.current.detail?.capture_id).toBe(second.capture_id));
  await act(async()=>pending.resolve(first));
  expect(hook.result.current.detail?.capture_id).toBe(second.capture_id);
});
it('distinguishes eviction and revokes replay authority immediately on 401',async()=>{
  const service=api();service.detail=vi.fn().mockRejectedValueOnce(new ApiError('not_found',404)).mockRejectedValueOnce(new ApiError('unauthorized',401));
  const invalidate=vi.fn();const hook=renderHook(()=>useHistory(service,{},true,invalidate,()=>{}));
  const summary=listFixture().items[0];if(!summary)throw Error('fixture');
  act(()=>hook.result.current.select(summary));await waitFor(()=>expect(hook.result.current.detailState).toBe('evicted'));
  expect(invalidate).toHaveBeenCalledTimes(1);invalidate.mockClear();
  act(()=>hook.result.current.select(summary));await waitFor(()=>expect(hook.result.current.forbidden).toBe(true));
  expect(invalidate).toHaveBeenCalledTimes(1);expect(hook.result.current.message).toContain('expired');
});
it('does not create polling work while updates are paused',async()=>{
  const service=api();const hook=renderHook(()=>useHistory(service,{},true,()=>{},()=>{}));
  await act(async()=>{});expect(service.list).not.toHaveBeenCalled();expect(service.stats).not.toHaveBeenCalled();hook.unmount();
});
it('invalidates a 401 immediately even when the other poll read is still pending',async()=>{
  const service=api();service.stats=vi.fn().mockRejectedValue(new ApiError('unauthorized',401));
  service.list=vi.fn().mockImplementation(()=>new Promise(()=>{}));
  const invalidate=vi.fn();const hook=renderHook(()=>useHistory(service,{},false,invalidate,()=>{}));
  await act(async()=>{});
  expect(invalidate).toHaveBeenCalledTimes(1);expect(hook.result.current.forbidden).toBe(true);hook.unmount();
});
it('preserves a pending confirmation and its outcome identity through ordinary polling cycles',async()=>{
    const service=api();let resolve:(value:OutcomeReply)=>void=()=>{};
    const pending=new Promise<OutcomeReply>(value=>{resolve=value;});
    const replay=new ReplayController(replayApi(vi.fn().mockReturnValue(pending)),()=>0);
    const invalidate=vi.fn(()=>replay.invalidate());
    const hook=renderHook(({paused})=>useHistory(service,{},paused,invalidate,row=>replay.select(row)),{initialProps:{paused:true}});
    const summary=listFixture().items[0];if(!summary)throw Error('fixture');
    act(()=>hook.result.current.select(summary));
    await waitFor(()=>expect(hook.result.current.detail).not.toBeNull());
    await replay.checkAvailability();await replay.prepare();replay.acknowledge(true);
    act(()=>hook.rerender({paused:false}));
    await waitFor(()=>expect(service.stats).toHaveBeenCalledTimes(1));
    expect(service.list).toHaveBeenCalledTimes(1);
    expect(invalidate).not.toHaveBeenCalled();
    expect(replay.getSnapshot().phase).toBe('reviewing_acknowledged');
    const confirming=replay.confirm();
    expect(replay.getSnapshot().phase).toBe('submitting');
    act(()=>hook.rerender({paused:true}));
    act(()=>hook.rerender({paused:false}));
    await waitFor(()=>expect(service.stats).toHaveBeenCalledTimes(2));
    expect(replay.getSnapshot().phase).toBe('submitting');
    resolve({schema_version:1,outcome:null});
    await confirming;
    expect(replay.getSnapshot().phase).toBe('outcome');
    hook.unmount();
});
it('runs only one subsequent poll pair after the previous cycle settles',async()=>{
  vi.useFakeTimers();
  try {
    const service=api();const hook=renderHook(()=>useHistory(service,{},false,()=>{},()=>{}));
    await act(async()=>{await Promise.resolve();await Promise.resolve();});
    expect(service.stats).toHaveBeenCalledTimes(1);expect(service.list).toHaveBeenCalledTimes(1);
    await act(async()=>{await vi.advanceTimersByTimeAsync(2_000);});
    expect(service.stats).toHaveBeenCalledTimes(2);expect(service.list).toHaveBeenCalledTimes(2);
    hook.unmount();
  }finally{vi.useRealTimers();}
});
it('cancels a held background detail before a filter restart launches the next read pair',async()=>{
  const service=api();let heldSignal:AbortSignal|undefined;
  service.detail=vi.fn().mockResolvedValueOnce(detailFixture()).mockImplementationOnce((_id:string,signal?:AbortSignal)=>new Promise<DetailReply>((_resolve,reject)=>{
    heldSignal=signal;signal?.addEventListener('abort',()=>reject(new ApiError('aborted')),{once:true});
  }));
  const hook=renderHook(({options,paused})=>useHistory(service,options,paused,()=>{},()=>{}),{initialProps:{options:{},paused:true}});
  const summary=listFixture().items[0];if(!summary)throw Error('fixture');
  act(()=>hook.result.current.select(summary));
  await waitFor(()=>expect(hook.result.current.detail).not.toBeNull());
  await act(async()=>{});
  act(()=>hook.rerender({options:{q:'changed'},paused:false}));
  await waitFor(()=>expect(heldSignal).toBeDefined());
  expect(heldSignal?.aborted).toBe(false);
  act(()=>hook.rerender({options:{q:'next'},paused:false}));
  await waitFor(()=>expect(heldSignal?.aborted).toBe(true));
  expect(service.stats).toHaveBeenCalledTimes(2);expect(service.list).toHaveBeenCalledTimes(2);
  hook.unmount();
});
it('clears an evicted background detail and replay authority but keeps a busy detail usable',async()=>{
  const summary=listFixture().items[0];if(!summary)throw Error('fixture');
  const evicted=api();evicted.detail=vi.fn().mockResolvedValueOnce(detailFixture()).mockRejectedValueOnce(new ApiError('not_found',404));
  const invalidate=vi.fn();const first=renderHook(({paused})=>useHistory(evicted,{},paused,invalidate,()=>{}),{initialProps:{paused:true}});
  act(()=>first.result.current.select(summary));await waitFor(()=>expect(first.result.current.detail).not.toBeNull());
  await act(async()=>{});
  act(()=>first.rerender({paused:false}));
  await waitFor(()=>expect(first.result.current.detailState).toBe('evicted'));
  expect(first.result.current.detail).toBeNull();expect(invalidate).toHaveBeenCalledTimes(1);first.unmount();

  const busy=api();busy.detail=vi.fn().mockResolvedValueOnce(detailFixture()).mockRejectedValueOnce(new ApiError('busy',503));
  const noInvalidate=vi.fn();const second=renderHook(({paused})=>useHistory(busy,{},paused,noInvalidate,()=>{}),{initialProps:{paused:true}});
  act(()=>second.result.current.select(summary));await waitFor(()=>expect(second.result.current.detail).not.toBeNull());
  await act(async()=>{});
  act(()=>second.rerender({paused:false}));
  await waitFor(()=>expect(second.result.current.detailState).toBe('busy'));
  expect(second.result.current.detail?.capture_id).toBe(summary.capture_id);expect(noInvalidate).not.toHaveBeenCalled();second.unmount();
});
it('coalesces polling and aborts on unmount instead of queuing new cycles behind a slow read',async()=>{
  vi.useFakeTimers();
  try {
    const service=api();service.stats=vi.fn().mockRejectedValue(new ApiError('busy',503));
    service.list=vi.fn().mockImplementation(()=>new Promise(()=>{}));
    const hook=renderHook(()=>useHistory(service,{},false,()=>{},()=>{}));
    await act(async()=>{await vi.advanceTimersByTimeAsync(10_000);});
    expect(service.stats).toHaveBeenCalledTimes(1);expect(service.list).toHaveBeenCalledTimes(1);
    const signal=vi.mocked(service.list).mock.calls[0]?.[1];hook.unmount();expect(signal?.aborted).toBe(true);
  }finally{vi.useRealTimers();}
});

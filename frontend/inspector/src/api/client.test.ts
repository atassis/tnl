import {expect,it,vi} from 'vitest';
import {InspectorClient,ApiError} from './client';
const token='a'.repeat(64);
it('uses only authenticated same-origin GETs with no redirects, storage credentials or cache',async()=>{
  const fetcher=vi.fn<typeof fetch>().mockResolvedValue(new Response(JSON.stringify({schema_version:1,records:0,tunnel:{generation:null,connected:false}}),{headers:{'Content-Type':'application/json'}}));
  const client=new InspectorClient(token,fetcher);
  await expect(client.stats()).resolves.toMatchObject({records:0});
  expect(fetcher).toHaveBeenCalledTimes(1);
  const [url,init]=fetcher.mock.calls[0]??[];
  expect(new URL(String(url),location.origin).origin).toBe(location.origin);
  expect(init).toMatchObject({method:'GET',cache:'no-store',credentials:'omit',redirect:'error',mode:'same-origin'});
  expect(new Headers(init?.headers).get('Authorization')).toBe(`Bearer ${token}`);
});
it('handles generic HTTP status before attempting success JSON validation',async()=>{
  for(const [status,kind]of [[401,'unauthorized'],[404,'not_found'],[503,'busy']] as const){
    const client=new InspectorClient(token,vi.fn<typeof fetch>().mockResolvedValue(new Response('<html>not JSON</html>',{status})));
    await expect(client.stats()).rejects.toMatchObject({kind,status});
  }
});
it('rejects malformed success DTOs, wrong versions and oversized bodies',async()=>{
  for(const body of ['{}','{"schema_version":2,"records":0,"tunnel":{"generation":null,"connected":false}}',' '.repeat(65_537)]){
    const client=new InspectorClient(token,vi.fn<typeof fetch>().mockResolvedValue(new Response(body,{headers:{'Content-Type':'application/json'}})));
    await expect(client.stats()).rejects.toMatchObject({kind:'invalid_response'});
  }
});
it('validates explicit replay confirmation rejection bodies before classifying their HTTP status',async()=>{
  const fetcher=vi.fn<typeof fetch>().mockResolvedValue(new Response(JSON.stringify({schema_version:1,error:'invalid_confirmation'}),{status:400,headers:{'Content-Type':'application/json'}}));
  const client=new InspectorClient(token,fetcher);
  await expect(client.confirm('00000000000000000000000000',{schema_version:1,replay_id:'00000000000000000000000001',confirmation_token:'x',confirm_side_effects:true})).rejects.toMatchObject({kind:'http',status:400,replayReason:'invalid_confirmation'});
});
it('revocation stops further requests and never exposes transport error strings',async()=>{
  const fetcher=vi.fn<typeof fetch>().mockRejectedValue(new Error('PRIVATE_CAPABILITY_CANARY'));
  const client=new InspectorClient(token,fetcher);
  let caught: unknown;
  try{await client.stats();}catch(error){caught=error;}
  expect(caught).toBeInstanceOf(ApiError);expect(String(caught)).not.toContain('CANARY');
  client.revoke();await expect(client.stats()).rejects.toMatchObject({kind:'unauthorized'});
  expect(fetcher).toHaveBeenCalledTimes(1);
});
it('rejects noncanonical capture IDs without issuing a request',async()=>{
  const fetcher=vi.fn<typeof fetch>();const client=new InspectorClient(token,fetcher);
  await expect(client.detail('../other')).rejects.toMatchObject({kind:'invalid_response'});
  expect(fetcher).not.toHaveBeenCalled();
});
it('does not issue a read whose caller has already cancelled',async()=>{
  const controller=new AbortController();controller.abort();
  const fetcher=vi.fn<typeof fetch>();const client=new InspectorClient(token,fetcher);
  await expect(client.stats(controller.signal)).rejects.toMatchObject({kind:'aborted'});
  expect(fetcher).not.toHaveBeenCalled();
});
it('cancels unread generic HTTP bodies instead of leaving streams alive',async()=>{
  const cancelled=vi.fn();
  const body=new ReadableStream<Uint8Array>({cancel:cancelled});
  const client=new InspectorClient(token,vi.fn<typeof fetch>().mockResolvedValue(new Response(body,{status:503})));
  await expect(client.stats()).rejects.toMatchObject({kind:'busy'});
  expect(cancelled).toHaveBeenCalledTimes(1);
});
it('bounds a stalled request independently and never retries it',async()=>{
  vi.useFakeTimers();
  try {
    const fetcher=vi.fn<typeof fetch>().mockImplementation(()=>new Promise<Response>(()=>{}));
    const client=new InspectorClient(token,fetcher);
    const result=expect(client.stats()).rejects.toMatchObject({kind:'network'});
    await vi.advanceTimersByTimeAsync(3001);await result;
    expect(fetcher).toHaveBeenCalledTimes(1);
  }finally{vi.useRealTimers();}
});
it.each([[404,'not_retained'],[503,'busy']] as const)('validates replay mutation rejection %i before generic classification',async(status,reason)=>{
  const client=new InspectorClient(token,vi.fn<typeof fetch>().mockResolvedValue(new Response(JSON.stringify({schema_version:1,error:reason}),{status,headers:{'Content-Type':'application/json'}})));
  await expect(client.confirm('00000000000000000000000000',{schema_version:1,replay_id:'00000000000000000000000001',confirmation_token:'x',confirm_side_effects:true})).rejects.toMatchObject({kind:'http',status,replayReason:reason});
});
it.each([404,503])('keeps malformed mutation %i responses uncertain and generic GET classification intact',async status=>{
  for(const body of ['{}','not JSON']) {
    const client=new InspectorClient(token,vi.fn<typeof fetch>().mockImplementation(async()=>new Response(body,{status,headers:{'Content-Type':'application/json'}})));
    await expect(client.confirm('00000000000000000000000000',{schema_version:1,replay_id:'00000000000000000000000001',confirmation_token:'x',confirm_side_effects:true})).rejects.toMatchObject({kind:'invalid_response',replayReason:null});
    await expect(client.stats()).rejects.toMatchObject({kind:status===404?'not_found':'busy',replayReason:null});
  }
});
it('revokes on mutation 401 before reading even a valid replay error body',async()=>{
  const fetcher=vi.fn<typeof fetch>().mockResolvedValue(new Response('{"schema_version":1,"error":"busy"}',{status:401,headers:{'Content-Type':'application/json'}}));
  const client=new InspectorClient(token,fetcher);
  await expect(client.prepare('00000000000000000000000000')).rejects.toMatchObject({kind:'unauthorized'});
  await expect(client.stats()).rejects.toMatchObject({kind:'unauthorized'});expect(fetcher).toHaveBeenCalledTimes(1);
});

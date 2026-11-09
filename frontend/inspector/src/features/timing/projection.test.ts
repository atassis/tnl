import {expect, it} from 'vitest';
import type {Timing} from '../../api/generated/detail';
import {projectTiming, validTiming} from './projection';
function timing(): Timing {
  const idle = {outcome:'not_started' as const, start_offset_us:null, end_offset_us:null};
  return {schema_version:1,scope:'client_backend',availability:'available',reason:null,snapshot:{
    as_of_offset_us:100,terminal:false,
    intervals:{metadata:idle,request_head:idle,resolve:idle,connect:idle,
      request_write:{outcome:'active',start_offset_us:10,end_offset_us:null},
      response_read:{outcome:'active',start_offset_us:20,end_offset_us:null},response_write:idle,upgraded:idle},
    backend_first_byte_offset_us:30,backend_final_headers_offset_us:50,
    request_framing_end_offset_us:null,response_framing_end_offset_us:null,request_framing_valid:false,response_framing_valid:false,
  }};
}
it('projects overlapping observed intervals on one axis without summing or extrapolating',()=>{
  const value=timing();expect(validTiming(value)).toBe(true);
  const projected=projectTiming(value);expect(projected).not.toBeNull();
  const upload=projected?.spans.find(span=>span.phase==='request_write');
  const read=projected?.spans.find(span=>span.phase==='response_read');
  expect(upload?.x).toBe(100);expect(upload?.width).toBe(900);expect(upload?.durationUs).toBe(90);
  expect(read?.x).toBe(200);expect(read?.durationUs).toBe(80);
  expect(projected?.asOfUs).toBe(100);
});
it('rejects contradictory availability and malformed temporal state',()=>{
  const a=timing();a.availability='unavailable';expect(validTiming(a)).toBe(false);
  for (const offset of [-1,NaN,Infinity,0.5,9_007_199_254_740_992]) {
    const a=timing();if(!a.snapshot)throw Error('fixture');a.snapshot.as_of_offset_us=offset;
    expect(validTiming(a)).toBe(false);
  }
  const b=timing();if(!b.snapshot)throw Error('fixture');b.snapshot.terminal=true;
  expect(validTiming(b)).toBe(false);
  const c=timing();if(!c.snapshot)throw Error('fixture');c.snapshot.intervals.connect={outcome:'completed',start_offset_us:80,end_offset_us:70};
  expect(validTiming(c)).toBe(false);
  const d=timing();if(!d.snapshot)throw Error('fixture');d.snapshot.request_framing_valid=true;expect(validTiming(d)).toBe(false);
  const e=timing();if(!e.snapshot)throw Error('fixture');e.snapshot.backend_first_byte_offset_us=null;expect(validTiming(e)).toBe(false);
});
it('keeps absent, skipped and observed zero duration distinct',()=>{
  const unavailable: Timing={schema_version:1,scope:'client_backend',availability:'unavailable',reason:'not_observed',snapshot:null};
  expect(validTiming(unavailable)).toBe(true);expect(projectTiming(unavailable)).toBeNull();
  const a=timing();if(!a.snapshot)throw Error('fixture');
  a.snapshot.intervals.resolve={outcome:'skipped',start_offset_us:null,end_offset_us:null};
  a.snapshot.intervals.connect={outcome:'completed',start_offset_us:0,end_offset_us:0};
  const spans=projectTiming(a)?.spans;
  expect(spans?.find(span=>span.phase==='resolve')?.durationUs).toBeNull();
  expect(spans?.find(span=>span.phase==='connect')?.durationUs).toBe(0);
});

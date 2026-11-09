import {afterEach,expect,it} from 'vitest';
import {cleanup,render,screen} from '@testing-library/react';
import type {Timing} from '../../api/generated/detail';
import {Timeline} from './Timeline';
afterEach(cleanup);
it('makes absent timing explicit rather than drawing fabricated zero spans',()=>{
  const timing:Timing={schema_version:1,scope:'client_backend',availability:'unavailable',reason:'replay_not_instrumented',snapshot:null};
  const {container}=render(<Timeline timing={timing}/>);
  expect(screen.getByText(/replay not instrumented/)).toBeTruthy();expect(container.querySelector('svg')).toBeNull();
});
it('provides an accessible table and numeric SVG without inline CSS',()=>{
  const idle={outcome:'not_started' as const,start_offset_us:null,end_offset_us:null};
  const timing:Timing={schema_version:1,scope:'client_backend',availability:'available',reason:null,snapshot:{as_of_offset_us:100,terminal:false,intervals:{metadata:idle,request_head:idle,resolve:idle,connect:{outcome:'completed',start_offset_us:0,end_offset_us:0},request_write:idle,response_read:{outcome:'active',start_offset_us:20,end_offset_us:null},response_write:idle,upgraded:idle},backend_first_byte_offset_us:30,backend_final_headers_offset_us:null,request_framing_end_offset_us:null,response_framing_end_offset_us:null,request_framing_valid:false,response_framing_valid:false}};
  const {container}=render(<Timeline timing={timing}/>);
  expect(screen.getByRole('table',{name:'Timing intervals'})).toBeTruthy();expect(screen.getByRole('img',{name:'Client-to-backend timing'})).toBeTruthy();
  expect(screen.getByText('0.000 ms')).toBeTruthy();expect(screen.getByText(/not distributed/)).toBeTruthy();
  expect(container.querySelector('[style],foreignObject,script')).toBeNull();
  for(const rect of container.querySelectorAll('rect')){expect(Number(rect.getAttribute('x'))).toBeGreaterThanOrEqual(0);expect(Number(rect.getAttribute('width'))).toBeLessThanOrEqual(1000);}
});

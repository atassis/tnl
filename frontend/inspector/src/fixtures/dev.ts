import type {InspectorApi} from '../app/App';
import {ApiError} from '../api/client';
import {detailFixture, listFixture, statsFixture} from './contracts';
export function syntheticApi(): InspectorApi {
  const unavailable = {state:'disabled' as const,reason:'disabled' as const,request_body_bytes:null,raw_expires_at_unix_ms:null};
  return {
    stats: async () => statsFixture(), list: async () => listFixture(),
    detail: async () => ({...detailFixture(), replay:{...unavailable,eligible:false}}),
    availability: async () => ({schema_version:1,availability:unavailable}),
    prepare: async () => {throw new ApiError('http',403,'disabled');},
    confirm: async () => {throw new ApiError('http',403,'disabled');},
    outcome: async () => ({schema_version:1,outcome:null}),
  };
}

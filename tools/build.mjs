import {spawnSync} from 'node:child_process';
import {root} from './process.mjs';
await import('./build-inspector.mjs');
const profile = process.argv[2];
if(!['release','dev'].includes(profile)) throw new Error('build profile must be release or dev');
const result = spawnSync('cargo',['build','--locked','--workspace',...(profile==='release'?['--release']:[])],{
  cwd:root,stdio:'inherit',env:{...process.env,CARGO_BUILD_JOBS:'1',CARGO_PROFILE_DEV_DEBUG:'0',CARGO_PROFILE_TEST_DEBUG:'0'},
});
if(result.error) throw result.error;
if(result.status!==0) process.exit(result.status ?? 1);

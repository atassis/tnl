import {spawnSync} from 'node:child_process';
import {resolve} from 'node:path';
import {cargo,root} from './process.mjs';
const messages = cargo(['build','--locked','-p','tnl','--message-format=json']).trim().split('\n').map(line=>JSON.parse(line));
const artifact = messages.find(message=>message.reason==='compiler-artifact' && message.target.src_path===resolve(root,'crates/tnl/build.rs'));
if(artifact?.filenames?.length!==1) throw new Error('Cargo did not identify the actual inspector build guard');
const result=spawnSync(process.execPath,['--test','tools/inspector-guard.test.mjs'],{
  cwd:root,stdio:'inherit',env:{...process.env,TNL_INSPECTOR_BUILD_GUARD:artifact.filenames[0]},timeout:60000});
if(result.error) throw result.error;
if(result.status!==0) process.exit(result.status??1);

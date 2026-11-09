import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
import {inputRules,createManifest} from './inspector-manifest.mjs';
export async function fixture(fn) {
  const root = await mkdtemp(join(tmpdir(),'tnl-manifest-'));
  const output = join(root,'crates/tnl/src/inspection/assets');
  async function put(path,bytes='input') { if(path.startsWith('output/')) path='crates/tnl/src/inspection/assets/'+path.slice(7);await mkdir(dirname(join(root,path)),{recursive:true});await writeFile(join(root,path),bytes);}
  try {
    for (const path of inputRules.files) await put(path);
    for (const path of inputRules.directories) await put(path+'/source.txt');
    for (const path of ['index.html','assets/app-123.js','assets/app-123.css']) await put('output/'+path,'asset');
    const manifest = await createManifest(root,output);
    await put('output/manifest.json',JSON.stringify(manifest));
    await fn({root,output,put,manifest});
  } finally {await rm(root,{recursive:true,force:true});}
}

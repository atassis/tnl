import {mkdir, readFile, writeFile, readdir, copyFile, rm} from 'node:fs/promises';
import {join} from 'node:path';
import {nodeTool} from './process.mjs';
import {createManifest,verifyManifest,defaultPaths,rejectSymlinks} from './inspector-manifest.mjs';
const {root,output} = defaultPaths();
await rejectSymlinks(root,'frontend/inspector/src/api/generated');
await rejectSymlinks(root,'frontend/inspector/dist');
await rejectSymlinks(root,'crates/tnl/src/inspection/assets');
await import('./generate-inspector-contract.mjs');
nodeTool('typescript','tsc',['--noEmit']);
nodeTool('vite','vite',['build']);
const dist = join(root,'frontend/inspector/dist');
// Validate the complete staged output before replacing previously built assets.
const manifest = await createManifest(root,dist);
await mkdir(output,{recursive:true});
for(const name of await readdir(output)) await rm(join(output,name),{recursive:true,force:true});
for(const asset of manifest.assets) {
  const destination = join(output,asset.path);
  await mkdir(join(destination,'..'),{recursive:true});
  await copyFile(join(dist,asset.path),destination);
}
await writeFile(join(output,'manifest.json'),JSON.stringify(manifest,null,2)+'\n');
await verifyManifest(root,output);
console.log('Embedded inspector assets:',manifest.assets.map(a=>`${a.path} ${a.size} bytes`).join(', '));

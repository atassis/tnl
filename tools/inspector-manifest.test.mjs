import {fixture} from './test-fixture.mjs';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, mkdir, writeFile, readFile, rm, symlink} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
import {createManifest,verifyManifest} from './inspector-manifest.mjs';
test('valid deterministic manifest accepts exact source and assets',()=>fixture(async({root,output})=>{
  assert.deepEqual(await createManifest(root,output),await createManifest(root,output));
  await assert.doesNotReject(verifyManifest(root,output));
}));
for (const change of ['edit','add','remove','asset-edit','asset-remove','unknown','traversal','mime','symlink-source','symlink-output','oversize']) {
  test(`rejects ${change}`,()=>fixture(async({root,output,put,manifest})=>{
    if(change==='edit') await put('frontend/inspector/source.txt','edited');
    if(change==='add') await put('frontend/inspector/new.txt');
    if(change==='remove') await rm(join(root,'frontend/inspector/source.txt'));
    if(change==='asset-edit') await put('output/assets/app-123.js','edited');
    if(change==='asset-remove') await rm(join(output,'assets/app-123.js'));
    if(change==='unknown') await put('output/extra.map','invalid');
    if(change==='traversal') {manifest.assets[0].path='../escape.js';await put('output/manifest.json',JSON.stringify(manifest));}
    if(change==='mime') {manifest.assets[0].mime='application/evil';await put('output/manifest.json',JSON.stringify(manifest));}
    if(change==='symlink-source') await symlink(join(root,'Cargo.toml'),join(root,'frontend/inspector/link'));
    if(change==='symlink-output') {await rm(join(output,'assets/app-123.js'));await symlink(join(root,'Cargo.toml'),join(output,'assets/app-123.js'));}
    if(change==='oversize') await put('output/assets/app-123.js',Buffer.alloc(512*1024+1));
    await assert.rejects(verifyManifest(root,output));
  }));
}

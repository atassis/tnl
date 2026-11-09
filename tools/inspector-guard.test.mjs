import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdir,rm,symlink} from 'node:fs/promises';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import {fixture} from './test-fixture.mjs';
const guard = process.env.TNL_INSPECTOR_BUILD_GUARD;
assert(guard,'explicit compiled Cargo build guard required');
async function run(root) {
  await mkdir(join(root,'crates/tnl'),{recursive:true});
  await mkdir(join(root,'guard-out'),{recursive:true});
  return spawnSync(guard,[],{encoding:'utf8',timeout:5000,env:{...process.env,
    CARGO_MANIFEST_DIR:join(root,'crates/tnl'),OUT_DIR:join(root,'guard-out')}});
}
test('actual compiled Cargo guard accepts a valid independent fixture',()=>fixture(async({root})=>{
  const result=await run(root);assert.equal(result.status,0,result.stderr);
}));
for(const change of ['source-edit','source-add','source-remove','asset-edit','asset-remove','manifest-missing','manifest-path','mime','symlink','oversize']) {
  test(`actual compiled Cargo guard rejects ${change}`,()=>fixture(async({root,output,put,manifest})=>{
    if(change==='source-edit') await put('frontend/inspector/source.txt','edit');
    if(change==='source-add') await put('frontend/inspector/add.ts');
    if(change==='source-remove') await rm(join(root,'frontend/inspector/source.txt'));
    if(change==='asset-edit') await put('output/assets/app-123.js','edit');
    if(change==='asset-remove') await rm(join(output,'assets/app-123.js'));
    if(change==='manifest-missing') await rm(join(output,'manifest.json'));
    if(change==='manifest-path') {manifest.inputs[0].path='../secret';await put('output/manifest.json',JSON.stringify(manifest));}
    if(change==='mime') {manifest.assets[0].mime='application/evil';await put('output/manifest.json',JSON.stringify(manifest));}
    if(change==='symlink') {await rm(join(output,'assets/app-123.js'));await symlink(join(root,'Cargo.toml'),join(output,'assets/app-123.js'));}
    if(change==='oversize') await put('output/assets/app-123.js',Buffer.alloc(512*1024+1));
    const result=await run(root);assert.notEqual(result.status,0);
    assert.match(result.stderr,/ui:prepare/);
  }));
}

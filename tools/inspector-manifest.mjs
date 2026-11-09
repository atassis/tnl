import {readFile, readdir, lstat} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {join, isAbsolute, sep} from 'node:path';
import {fileURLToPath} from 'node:url';
export const inputRules = JSON.parse(await readFile(new URL('./inspector-inputs.json',import.meta.url)));
export const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const canonical = path => path.split(sep).join('/');
export async function rejectSymlinks(root, prefix) {
  if (isAbsolute(prefix) || prefix.split('/').some(part=>part==='..')) throw new Error('unsafe path');
  let path = root;
  for (const part of prefix.split('/').filter(Boolean)) {
    path = join(path,part);
    try {
      if ((await lstat(path)).isSymbolicLink()) throw new Error(`symlink forbidden: ${prefix}`);
    } catch (error) { if(error.code!=='ENOENT') throw error; }
  }
}
async function filesUnder(root, prefix, excludes = []) {
  if (excludes.includes(prefix)) return [];
  await rejectSymlinks(root,prefix);
  const path = join(root, prefix);
  const stat = await lstat(path);
  if (stat.isSymbolicLink()) throw new Error(`symlink forbidden: ${prefix}`);
  if (stat.isFile()) return [prefix];
  if (!stat.isDirectory()) throw new Error(`unsupported input: ${prefix}`);
  const files = [];
  for (const name of (await readdir(path)).sort()) files.push(...await filesUnder(root,canonical(join(prefix,name)),excludes));
  return files;
}
export async function sourceFiles(root) {
  const files = [];
  for (const prefix of [...inputRules.files,...inputRules.directories]) {
    files.push(...await filesUnder(root,prefix,inputRules.exclude));
  }
  return [...new Set(files)].sort((a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b)));
}
export async function createManifest(root, output) {
  const inputs = [];
  for (const path of await sourceFiles(root)) inputs.push({path,sha256:digest(await readFile(join(root,path)))});
  const assets = [];
  let js = 0, css = 0, total = 0;
  for (const path of await filesUnder(output,'')) {
    if (path === 'manifest.json') continue;
    const mime = path === 'index.html' ? 'text/html; charset=utf-8'
      : /^assets\/[A-Za-z0-9_-]+\.js$/.test(path) ? 'text/javascript; charset=utf-8'
      : /^assets\/[A-Za-z0-9_-]+\.css$/.test(path) ? 'text/css; charset=utf-8' : null;
    if (!mime) throw new Error(`unsupported asset: ${path}`);
    const bytes = await readFile(join(output,path));
    if (path.endsWith('.js')) js += bytes.length;
    if (path.endsWith('.css')) css += bytes.length;
    total += bytes.length;
    assets.push({path,mime,size:bytes.length,sha256:digest(bytes)});
  }
  if (!assets.some(a=>a.path==='index.html') || !assets.some(a=>a.path.endsWith('.js')) || !assets.some(a=>a.path.endsWith('.css'))) throw new Error('HTML, JS and CSS are required');
  if (js>512*1024 || css>128*1024 || total>1024*1024) throw new Error(`asset budget exceeded: js=${js} css=${css} total=${total}`);
  return {format_version:1,inputs,assets:assets.sort((a,b)=>Buffer.compare(Buffer.from(a.path),Buffer.from(b.path)))};
}
export async function verifyManifest(root, output) {
  await rejectSymlinks(output,'manifest.json');
  if ((await lstat(join(output,'manifest.json'))).size>65536) throw new Error('manifest budget exceeded');
  const manifest = JSON.parse(await readFile(join(output,'manifest.json'),'utf8'));
  const expected = await createManifest(root,output);
  if (JSON.stringify(manifest)!==JSON.stringify(expected)) throw new Error('inspector assets are stale or modified; run corepack pnpm ui:prepare');
  return manifest;
}
export function defaultPaths() {
  return {root:fileURLToPath(new URL('../',import.meta.url)),output:fileURLToPath(new URL('../crates/tnl/src/inspection/assets/',import.meta.url))};
}

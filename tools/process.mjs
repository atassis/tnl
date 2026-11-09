import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import {dirname,join} from 'node:path';
export const root = fileURLToPath(new URL('../', import.meta.url));
export const frontendRequire = createRequire(new URL('../frontend/inspector/package.json', import.meta.url));
export function cargo(args) {
  try { return execFileSync('cargo', args, {
    cwd: root, encoding: 'utf8', maxBuffer: 1024 * 1024, timeout: 600000,
    env: {...process.env, CARGO_BUILD_JOBS:'1', CARGO_PROFILE_DEV_DEBUG:'0', CARGO_PROFILE_TEST_DEBUG:'0'},
    stdio: ['ignore', 'pipe', 'inherit'],
  }); } catch(error) {
    if(args.includes('--message-format=json') && error.stdout) {
      for(const line of error.stdout.trim().split('\n')) {
        const message=JSON.parse(line);
        if(message.reason==='compiler-message') console.error(message.message.rendered);
      }
    }
    throw new Error(`Cargo command failed (status ${error.status}, signal ${error.signal})`);
  }
}
export function nodeTool(packageName, command, args) {
  const metadata = frontendRequire(packageName+'/package.json');
  const entry = join(dirname(frontendRequire.resolve(packageName+'/package.json')), metadata.bin[command]);
  execFileSync(process.execPath, [entry, ...args], {
    cwd: fileURLToPath(new URL('../frontend/inspector/', import.meta.url)),
    stdio: 'inherit', timeout: 180000, env: {...process.env,NODE_ENV:'production'},
  });
}

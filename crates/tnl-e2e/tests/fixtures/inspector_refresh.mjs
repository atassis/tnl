import {hasExited, stopChild} from "./browser_process.mjs";
import {spawn,spawnSync} from 'node:child_process';
import os from 'node:os';import fs from 'node:fs';import path from 'node:path';import http from 'node:http';import net from 'node:net';import assert from 'node:assert/strict';
process.umask(0o077);
const root=fs.mkdtempSync(path.join(process.platform==='win32'?os.tmpdir():'/tmp','tnl-inspector-refresh-'));
const clientBin=process.env.TNL_TEST_CLIENT_BIN,daemonBin=process.env.TNL_TEST_DAEMON_BIN;
assert(clientBin&&daemonBin,'explicit candidate binaries required');
const envBase={...process.env,HOME:root,TMPDIR:root,XDG_CONFIG_HOME:path.join(root,'config')};
for(const k of Object.keys(envBase))if(k.startsWith('TNL_')||k.startsWith('TNLD_'))delete envBase[k];
const delay=ms=>new Promise(r=>setTimeout(r,ms));const children=[];let ws;const received=[];
let publicRequestId;
const server=http.createServer(async(req,res)=>{const chunks=[];for await(const b of req)chunks.push(b);received.push({method:req.method,url:req.url,headers:req.headers,body:Buffer.concat(chunks).toString()});res.writeHead(200,{'content-type':'application/json','connection':'close'});res.end('{"token":"RESPONSE_BODY_CANARY","public":"ok"}');});
const listen=s=>new Promise(r=>s.listen(0,'127.0.0.1',()=>r(s.address().port)));
async function until(fn,message,tries=200){for(let n=0;n<tries;n++){const v=await fn();if(v)return v;await delay(50);}throw new Error(message);}
function child(name,args,env=envBase){const p=spawn(name,args,{env,stdio:['ignore','pipe','pipe']});children.push(p);let out='';p.on('error',()=>{out+='fixture process failed to start';});p.stdout.on('data',b=>out+=b);p.stderr.on('data',b=>out+=b);return {process:p,output:()=>out};}
try{
 const backend=await listen(server);const reserve=net.createServer();const port=await listen(reserve);await new Promise(r=>reserve.close(r));
 const tokens=path.join(root,'tokens.toml');assert(!fs.existsSync(tokens),'use a fresh fixture directory');
 const tokenResult=spawnSync(daemonBin,['token','add','browser-test','--tokens-file',tokens],{encoding:'utf8'});assert.equal(tokenResult.status,0,tokenResult.stderr);const token=tokenResult.stdout.trim();assert(token.startsWith('tnl_'));
 const daemonConfig=path.join(root,'daemon.toml');fs.writeFileSync(daemonConfig,`listen = "127.0.0.1:${port}"\npublic_url = "http://127.0.0.1:${port}"\nhostname_root = "t.test"\ntokens_file = ${JSON.stringify(tokens)}\n`,{mode:0o600});
 const daemon=child(daemonBin,['serve','--config',daemonConfig]);await until(async()=>{try{return(await fetch(`http://127.0.0.1:${port}/healthz`,{signal:AbortSignal.timeout(1000)})).status===200;}catch{return false;}},'fixture daemon failed');
 const config=path.join(root,'client.toml');fs.writeFileSync(config,`endpoint = "http://127.0.0.1:${port}"\ntoken = ${JSON.stringify(token)}\n`,{mode:0o600});const env={...envBase,TNL_CONFIG:config};delete env.TNL_TOKEN;delete env.TNL_ENDPOINT;
 const cli=child(clientBin,['http',String(backend),'browser','--inspect','--inspect-bodies','json','--replay-retain-secrets','--quiet'],env);
 const link=await until(()=>cli.output().match(/http:\/\/127\.0\.0\.1:\d+\/#token=[a-f0-9]{64}/)?.[0],'inspector URL missing');const url=new URL(link),cap=url.hash.slice('#token='.length);
 await until(async()=>{try{const r=await fetch(url.origin+'/api/v1/stats',{headers:{Authorization:'Bearer '+cap},signal:AbortSignal.timeout(1000)});return(await r.json()).tunnel.connected;}catch{return false;}},'tunnel did not connect');
 const requestPath='/canary?secret=RAW_QUERY_CANARY';const body='{"token":"RAW_BODY_CANARY","public":"<svg/onload=globalThis.XSS=1>"}';
 await new Promise((resolve,reject)=>{const q=http.request({host:'127.0.0.1',port,path:requestPath,method:'POST',headers:{host:'browser.t.test',authorization:'Bearer RAW_AUTH_CANARY',cookie:'session=RAW_COOKIE_CANARY','content-type':'application/json','content-length':Buffer.byteLength(body),'connection':'close'}},r=>{publicRequestId=r.headers['x-tnl-request-id'];r.resume();r.on('end',()=>r.statusCode===200?resolve():reject(new Error('original request status '+r.statusCode)));});q.setTimeout(5000,()=>q.destroy(new Error('original request deadline')));q.on('error',reject);q.end(body);});
 assert.equal(received.length,1);
 const browser=child(process.env.TNL_TEST_BROWSER_BIN??'/usr/bin/brave',['--headless=new','--no-first-run','--no-default-browser-check','--disable-background-networking','--remote-debugging-address=127.0.0.1','--remote-debugging-port=0','--user-data-dir='+path.join(root,'profile'),'about:blank']);
 const debugPort=await until(()=>{if(hasExited(browser.process))throw new Error('isolated browser exited ('+browser.process.signalCode+'): '+browser.output().slice(-2500));try{return Number(fs.readFileSync(path.join(root,'profile/DevToolsActivePort'),'utf8').split('\n')[0]);}catch{return false;}},'isolated browser failed');
 const pages=await(await fetch(`http://127.0.0.1:${debugPort}/json/list`,{signal:AbortSignal.timeout(5000)})).json();ws=new WebSocket(pages.find(p=>p.type==='page').webSocketDebuggerUrl);await new Promise((r,j)=>{const timer=setTimeout(()=>j(new Error('CDP open deadline')),5000);ws.addEventListener('open',()=>{clearTimeout(timer);r();},{once:true});ws.addEventListener('error',e=>{clearTimeout(timer);j(e);},{once:true});});
 let id=0;const pending=new Map(),exceptions=[],mutations=[];let confirmationToken;
 ws.addEventListener('message',e=>{const m=JSON.parse(e.data);if(m.id){const p=pending.get(m.id);if(p){pending.delete(m.id);m.error?p.reject(new Error(JSON.stringify(m.error))):p.resolve(m.result);}}if(m.method==='Runtime.exceptionThrown')exceptions.push(m.params.exceptionDetails.text);if(m.method==='Network.requestWillBeSent'&&m.params.request.url.startsWith(url.origin)&&m.params.request.method==='POST'){const req=m.params.request;mutations.push(new URL(req.url).pathname);if(req.url.endsWith('/confirm'))confirmationToken=JSON.parse(req.postData).confirmation_token;}});
 function cdp(method,params={}){return new Promise((resolve,reject)=>{const n=++id;const timer=setTimeout(()=>{pending.delete(n);reject(new Error('CDP deadline: '+method));},5000);pending.set(n,{resolve:v=>{clearTimeout(timer);resolve(v);},reject:e=>{clearTimeout(timer);reject(e);}});ws.send(JSON.stringify({id:n,method,params}));});}
 async function evaluate(expression){const r=await cdp('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});assert(!r.exceptionDetails,JSON.stringify(r.exceptionDetails));return r.result.value;}
 async function reload() {
   const loaded=new Promise((resolve,reject)=>{
     const listener=e=>{const m=JSON.parse(e.data);if(m.method==='Page.loadEventFired'){clearTimeout(timer);ws.removeEventListener('message',listener);resolve();}};
     const timer=setTimeout(()=>{ws.removeEventListener('message',listener);reject(new Error('reload deadline'));},5000);
     ws.addEventListener('message',listener);
   });
   await cdp('Page.reload');await loaded;
   await until(()=>evaluate('document.querySelectorAll("#rows tr").length === 1'),'authenticated history lost after F5');
   assert.equal(await evaluate('location.hash'),url.hash);
   assert.equal(await evaluate('localStorage.length+sessionStorage.length'),0);assert.equal(await evaluate('document.cookie'),'');
 }
 for(const method of ['Page.enable','Runtime.enable','Network.enable'])await cdp(method);
 await cdp('Page.navigate',{url:link});await until(()=>evaluate('document.querySelectorAll("#rows tr").length === 1'),'capture did not render');
 assert.equal(await evaluate('location.hash'),url.hash);assert.equal(await evaluate('localStorage.length+sessionStorage.length'),0);assert.equal(await evaluate('document.cookie'),'');
 await reload();
 await evaluate('document.querySelector("#rows button").click()');await until(()=>evaluate('document.getElementById("replay-review").hidden === false'),'capture not replay eligible');
 assert(publicRequestId,'public request ID missing');assert.equal(received[0].headers['x-tnl-request-id'],publicRequestId);assert.equal(await evaluate('JSON.parse(document.getElementById("detail").textContent).request_id'),publicRequestId);
 let dom=await evaluate('document.body.innerText');for(const canary of ['RAW_QUERY_CANARY','RAW_AUTH_CANARY','RAW_COOKIE_CANARY','RAW_BODY_CANARY','RESPONSE_BODY_CANARY',cap])assert(!dom.includes(canary),'secret in DOM: '+canary);
 assert.equal(await evaluate('document.querySelectorAll("svg").length'),0);assert.equal(await evaluate('typeof globalThis.XSS'),'undefined');
 await evaluate('document.getElementById("replay-review").click()');await until(()=>evaluate('document.getElementById("replay-preview").textContent.includes("Review ONE attempt")'),'prepare did not render');
 assert.equal(received.length,1,'prepare sent a backend request');assert.equal(await evaluate('document.getElementById("replay-confirm").disabled'),true);
 await evaluate('document.getElementById("replay-ack").click()');assert.equal(await evaluate('document.getElementById("replay-confirm").disabled'),false);
 await reload();
 assert.equal(received.length,1,'F5 must not execute a prepared/acknowledged replay');
 assert.equal(await evaluate('document.getElementById("replay-ack").checked'),false);
 assert.equal(await evaluate('document.getElementById("replay-confirm").disabled'),true);
 assert.equal(mutations.length,1,'F5 must not prepare or confirm automatically');
 await evaluate('document.querySelector("#rows button").click()');await until(()=>evaluate('document.getElementById("replay-review").hidden === false'),'capture unavailable after refresh');
 await evaluate('document.getElementById("replay-review").click()');await until(()=>evaluate('document.getElementById("replay-preview").textContent.includes("Review ONE attempt")'),'fresh explicit review missing');
 assert.equal(await evaluate('document.getElementById("replay-confirm").disabled'),true);
 await evaluate('document.getElementById("replay-ack").click()');
 await evaluate('document.getElementById("replay-confirm").click();document.getElementById("replay-confirm").click()');
 await until(()=>received.length===2,'confirmed replay did not arrive');await until(()=>evaluate('document.getElementById("replay-outcome").textContent.includes("response_received")'),'linked replay outcome missing');
 assert.equal(received[1].method,received[0].method);assert.equal(received[1].url,received[0].url);assert.equal(received[1].body,received[0].body);for(const header of ['authorization','cookie','host'])assert.equal(received[1].headers[header],received[0].headers[header]);
 await delay(2300);assert.equal(received.length,2,'polling or duplicate click replayed automatically');assert.equal(mutations.length,3,'unexpected mutation count');
 dom=await evaluate('document.body.innerText');for(const canary of ['RAW_QUERY_CANARY','RAW_AUTH_CANARY','RAW_COOKIE_CANARY','RAW_BODY_CANARY','RESPONSE_BODY_CANARY',cap,confirmationToken])assert(!dom.includes(canary),'secret/ticket in DOM');assert.deepEqual(exceptions,[]);
 console.log(JSON.stringify({browser:'isolated headless Brave / local CDP; unmodified browser security headers',fragment_retained_for_refresh:true,authenticated_F5:true,F5_clears_confirmation_and_never_replays:true,authenticated_captured_record:true,backend_public_capture_ID_equal:true,secrets_absent_from_DOM:true,HTML_canary_not_executed:true,prepare_sent_nothing:true,explicit_acknowledgement_required:true,original_method_query_auth_cookie_host_body_replayed:true,duplicate_click_and_polling_no_retry:true,linked_outcome_response_received:true,POST_paths:mutations,backend_requests:received.length,JS_exceptions:exceptions},null,2));
}finally{
 if(ws)ws.close();
 await Promise.all(children.map(p=>stopChild(p,p.spawnfile===clientBin?'SIGINT':'SIGTERM')));
 server.closeAllConnections();await new Promise(r=>server.close(r));
 fs.rmSync(root,{recursive:true,force:true});
}

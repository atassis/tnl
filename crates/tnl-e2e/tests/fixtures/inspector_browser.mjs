// Real embedded assets only: every scenario owns its backend, daemon, CLI and CDP profile.
import {hasExited, stopChild} from './browser_process.mjs';
import {spawn, spawnSync} from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import net from 'node:net';
import assert from 'node:assert/strict';

export const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
export async function until(fn, message, tries = 160) {
  for (let n = 0; n < tries; n++) { const value = await fn(); if (value) return value; await delay(50); }
  throw new Error(message);
}
export const htmlCanary = '<div data-capture-canary="literal">HTML_CANARY</div><svg/onload=globalThis.XSS=1>';
const secrets = ['RAW_QUERY_CANARY', 'RAW_AUTH_CANARY', 'RAW_COOKIE_CANARY', 'RAW_BODY_CANARY', 'RESPONSE_BODY_CANARY', 'RESPONSE_COOKIE_CANARY'];
const policy = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

export async function withInspector(scenario) {
  process.umask(0o077);
  // Deliberately short: Rust supplies a long TMPDIR to exercise browser socket-path isolation.
  const root = fs.mkdtempSync('/tmp/tnl-inspector-browser-');
  const clientBin = process.env.TNL_TEST_CLIENT_BIN, daemonBin = process.env.TNL_TEST_DAEMON_BIN;
  const browserBin = process.env.TNL_TEST_BROWSER_BIN;
  assert(clientBin && daemonBin && browserBin, 'explicit candidate CLI, daemon and browser required');
  for (const bin of [clientBin, daemonBin, browserBin]) fs.accessSync(bin, fs.constants.X_OK);
  const envBase = {...process.env, HOME: root, TMPDIR: root, XDG_CONFIG_HOME: path.join(root, 'config')};
  for (const key of Object.keys(envBase)) if (key.startsWith('TNL_') || key.startsWith('TNLD_')) delete envBase[key];
  const children = [], received = [], mutations = [], responses = [], exceptions = [], privateValues = [], reads = new Set();
  let ws, cdpId = 0;
  const pending = new Map(), requests = new Map(), wireBySocket = new WeakMap();
  const server = http.createServer(async (req, res) => {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    received.push({method: req.method, url: req.url, headers: req.headers, body: Buffer.concat(chunks), wire: Buffer.concat(wireBySocket.get(req.socket))});
    res.writeHead(200, {'content-type': 'application/json', connection: 'close', 'set-cookie': 'session=RESPONSE_COOKIE_CANARY'});
    res.end(JSON.stringify({token: 'RESPONSE_BODY_CANARY', public: htmlCanary, wrap: 'w'.repeat(320)}));
  });
  server.on('connection', socket => {
    const chunks = []; wireBySocket.set(socket, chunks);
    socket.on('data', chunk => chunks.push(Buffer.from(chunk)));
  });
  const listen = server => new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(server.address().port)));
  function child(bin, args, env = envBase) {
    // File-backed logs stay private and cannot block a child on an unread pipe.
    const log = path.join(root, `child-${children.length}.log`), fd = fs.openSync(log, 'w', 0o600);
    let process;
    try { process = spawn(bin, args, {env, stdio: ['ignore', fd, fd]}); } finally { fs.closeSync(fd); }
    children.push(process); process.on('error', () => {});
    return {process, output: () => fs.readFileSync(log, 'utf8')};
  }
  function cdp(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++cdpId;
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP deadline: ${method}`)); }, 5000);
      pending.set(id, {resolve: value => {clearTimeout(timer); resolve(value);}, reject: error => {clearTimeout(timer); reject(new Error(`${method}: ${error.message}`));}});
      ws.send(JSON.stringify({id, method, params}));
    });
  }
  async function evaluate(expression) {
    const result = await cdp('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true});
    assert(!result.exceptionDetails, 'browser evaluation failed: ' + result.exceptionDetails?.exception?.description);
    return result.result.value;
  }
  const buttonExpression = name => `[...document.querySelectorAll('button')].find(b => b.textContent.trim() === ${JSON.stringify(name)})`;
  async function click(name) {
    assert(await evaluate(`Boolean(${buttonExpression(name)} && !${buttonExpression(name)}.disabled)`), `enabled button missing: ${name}`);
    await evaluate(`${buttonExpression(name)}.click()`);
  }
  async function tab(name) { await evaluate(`document.querySelector('[role="tab"][id="detail-tab-${name}"]').click()`); }
  async function select(captureId) {
    await evaluate(`document.querySelector('[data-capture-id="${captureId}"]').click()`);
    await until(() => evaluate(`document.querySelector('[data-testid="capture-id"]')?.textContent === ${JSON.stringify(captureId)}`), 'selected capture did not render');
  }
  async function navigate(link) {
    await cdp('Page.navigate', {url: link});
    await until(() => evaluate('Boolean(document.querySelector(".request-list"))'), 'production application did not load');
  }
  async function reload() {
    // Unlike same-URL navigation this is an actual F5-equivalent document reload.
    const loaded = new Promise((resolve, reject) => {
      const listener = event => { if (JSON.parse(event.data).method === 'Page.loadEventFired') {clearTimeout(timer); ws.removeEventListener('message', listener); resolve();} };
      const timer = setTimeout(() => {ws.removeEventListener('message', listener); reject(new Error('reload deadline'));}, 5000);
      ws.addEventListener('message', listener);
    });
    await cdp('Page.reload'); await loaded;
    await until(() => evaluate('Boolean(document.querySelector(".request-list"))'), 'application missing after F5');
  }
  async function privacy() {
    await Promise.all([...reads]);
    const dom = await evaluate('document.documentElement.outerHTML');
    for (const secret of [...secrets, ...privateValues]) assert(!dom.includes(secret), 'private value leaked into DOM');
    assert.equal(await evaluate('localStorage.length + sessionStorage.length'), 0, 'browser storage must remain empty');
    assert.equal(await evaluate('document.cookie'), '');
    assert.deepEqual((await cdp('Network.getCookies')).cookies, [], 'no browser cookies, including HttpOnly');
    assert.equal(await evaluate('typeof globalThis.XSS'), 'undefined');
    assert.deepEqual(exceptions, [], 'unexpected runtime exception');
  }
  async function inertBody() {
    await until(() => evaluate('Boolean(document.querySelector("[data-testid=captured-body]"))'), 'captured body missing');
    const body = await evaluate(`(() => {const node = document.querySelector('[data-testid=captured-body]'); return {text: node.textContent, children: node.querySelectorAll('*').length};})()`);
    assert(body.text.includes('<div') && body.text.includes('<svg/onload=globalThis.XSS=1>'), 'HTML/SVG canary must be visible literally');
    assert.equal(body.children, 0, 'captured body must contain text only, not HTML/SVG elements');
    await privacy();
  }
  try {
    const backend = await listen(server), reserve = net.createServer(), port = await listen(reserve);
    await new Promise(resolve => reserve.close(resolve));
    const tokens = path.join(root, 'tokens.toml');
    const result = spawnSync(daemonBin, ['token', 'add', 'browser-test', '--tokens-file', tokens], {encoding: 'utf8', env: envBase, timeout: 5000});
    assert.equal(result.status, 0, 'create isolated fixture token');
    const token = result.stdout.trim(); assert(token.startsWith('tnl_')); privateValues.push(token);
    const daemonConfig = path.join(root, 'daemon.toml');
    fs.writeFileSync(daemonConfig, `listen = "127.0.0.1:${port}"\npublic_url = "http://127.0.0.1:${port}"\nhostname_root = "t.test"\ntokens_file = ${JSON.stringify(tokens)}\n`, {mode: 0o600});
    child(daemonBin, ['serve', '--config', daemonConfig]);
    await until(async () => {try {return (await fetch(`http://127.0.0.1:${port}/healthz`, {signal: AbortSignal.timeout(1000)})).status === 200;} catch {return false;}}, 'fixture daemon unavailable');
    const config = path.join(root, 'client.toml');
    fs.writeFileSync(config, `endpoint = "http://127.0.0.1:${port}"\ntoken = ${JSON.stringify(token)}\n`, {mode: 0o600});
    async function startClient(inspectorPort) {
      const cli = child(clientBin, ['http', String(backend), 'browser', '--inspect', '--inspect-bodies', 'json', '--replay-retain-secrets', '--quiet', ...(inspectorPort ? ['--inspect-port', String(inspectorPort)] : [])], {...envBase, TNL_CONFIG: config});
      const link = await until(() => cli.output().match(/http:\/\/127\.0\.0\.1:\d+\/#token=[a-f0-9]{64}/)?.[0], 'private inspector startup link missing');
      const url = new URL(link), cap = url.hash.slice('#token='.length); privateValues.push(cap);
      const api = (pathname, capability = cap) => fetch(url.origin + pathname, {headers: {Authorization: 'Bearer ' + capability}, signal: AbortSignal.timeout(1500)});
      await until(async () => {try {const response = await api('/api/v1/stats'); return response.ok && (await response.json()).tunnel.connected;} catch {return false;}}, 'tunnel did not connect');
      return {cli, link, url, cap, api};
    }
    const client = await startClient();
    async function capture(requestPath = '/canary?secret=RAW_QUERY_CANARY') {
      const body = Buffer.from(JSON.stringify({token: 'RAW_BODY_CANARY', public: htmlCanary, wrap: 'w'.repeat(320)}));
      let publicRequestId;
      await new Promise((resolve, reject) => {
        const request = http.request({host: '127.0.0.1', port, path: requestPath, method: 'POST', headers: {host: 'browser.t.test', authorization: 'Bearer RAW_AUTH_CANARY', cookie: 'session=RAW_COOKIE_CANARY', 'content-type': 'application/json', 'content-length': body.length, connection: 'close'}}, response => {
          publicRequestId = response.headers['x-tnl-request-id']; response.resume();
          response.on('end', () => response.statusCode === 200 ? resolve() : reject(new Error('original request failed')));
        });
        request.setTimeout(5000, () => request.destroy(new Error('public request deadline'))); request.on('error', reject); request.end(body);
      });
      assert(publicRequestId, 'canonical public request ID missing');
      const record = await until(async () => {
        const response = await client.api('/api/v1/requests?limit=50'); if (!response.ok) return false;
        const list = await response.json(), row = list.items.find(item => item.request_id === publicRequestId);
        if (!row) return false;
        const detail = await client.api('/api/v1/requests/' + row.capture_id); if (!detail.ok) return false;
        const value = await detail.json(); return value.state === 'complete' ? value : false;
      }, 'completed API capture missing');
      assert.equal(received.at(-1).headers['x-tnl-request-id'], publicRequestId);
      assert.equal(record.request_id, publicRequestId);
      assert(!JSON.stringify(record).includes('RAW_BODY_CANARY'), 'API must be redacted');
      for (const secret of secrets) assert(!JSON.stringify(record).includes(secret), 'API leaked a canary');
      return {record, publicRequestId};
    }
    const first = await capture();
    const browser = child(browserBin, ['--headless=new', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', '--user-data-dir=' + path.join(root, 'profile'), 'about:blank']);
    const debugPort = await until(() => {
      if (hasExited(browser.process)) throw new Error('isolated browser exited before CDP startup');
      try {return Number(fs.readFileSync(path.join(root, 'profile/DevToolsActivePort'), 'utf8').split('\n')[0]);} catch {return false;}
    }, 'isolated browser CDP unavailable');
    const pages = await (await fetch(`http://127.0.0.1:${debugPort}/json/list`, {signal: AbortSignal.timeout(5000)})).json();
    ws = new WebSocket(pages.find(page => page.type === 'page').webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('CDP open deadline')), 5000);
      ws.addEventListener('open', () => {clearTimeout(timer); resolve();}, {once: true});
      ws.addEventListener('error', () => {clearTimeout(timer); reject(new Error('CDP open failed'));}, {once: true});
    });
    ws.addEventListener('message', event => {
      const message = JSON.parse(event.data);
      if (message.id) {const waiter = pending.get(message.id); if (waiter) {pending.delete(message.id); message.error ? waiter.reject(new Error(message.error.message)) : waiter.resolve(message.result);}}
      if (message.method === 'Runtime.exceptionThrown') exceptions.push(message.params.exceptionDetails.text);
      if (message.method === 'Network.requestWillBeSent') {
        const request = message.params.request;
        if (!request.url.startsWith(client.url.origin + '/')) return;
        const pathname = new URL(request.url).pathname;
        requests.set(message.params.requestId, {pathname, method: request.method});
        if (request.method === 'POST') {
          mutations.push(pathname);
          if (pathname.endsWith('/confirm')) privateValues.push(JSON.parse(request.postData).confirmation_token);
        }
      }
      if (message.method === 'Network.responseReceived') {
        const request = requests.get(message.params.requestId);
        if (request) responses.push({...request, status: message.params.response.status, headers: message.params.response.headers, mime: message.params.response.mimeType});
      }
      if (message.method === 'Network.loadingFinished' && requests.get(message.params.requestId)?.pathname.endsWith('/prepare')) {
        const read = cdp('Network.getResponseBody', {requestId: message.params.requestId}).then(result => {
          const preview = JSON.parse(result.body).preview; if (preview?.confirmation_token) privateValues.push(preview.confirmation_token);
        });
        // Keep fulfilled/rejected reads until privacy() awaits them; avoid unhandled
        // rejections from a detached finally promise during document navigation.
        reads.add(read); void read.catch(() => {});
      }
    });
    for (const method of ['Page.enable', 'Runtime.enable', 'Network.enable']) await cdp(method);
    await cdp('Page.addScriptToEvaluateOnNewDocument', {source: `globalThis.cspViolations = []; document.addEventListener('securitypolicyviolation', event => globalThis.cspViolations.push({directive: event.effectiveDirective, blocked: event.blockedURI}));`});
    await navigate(client.link);
    await until(() => evaluate('document.querySelectorAll("[data-capture-id]").length === 1'), 'authenticated capture did not render');
    await select(first.record.capture_id);
    assert.equal(await evaluate('document.querySelector("[data-testid=request-id]").textContent'), first.publicRequestId);
    assert.equal(await evaluate('document.querySelector("[data-testid=capture-id]").textContent'), first.record.capture_id);
    const documentResponse = responses.find(response => response.pathname === '/' && response.status === 200);
    assert(documentResponse, 'Rust-served production document missing');
    const headers = Object.fromEntries(Object.entries(documentResponse.headers).map(([key, value]) => [key.toLowerCase(), value]));
    assert.equal(headers['content-security-policy'], policy);
    assert.equal(headers['cache-control'], 'no-store'); assert.equal(headers['referrer-policy'], 'no-referrer');
    assert.equal(headers['x-frame-options'], 'DENY'); assert.equal(headers['x-content-type-options'], 'nosniff');
    assert(responses.some(response => response.mime === 'text/css' && response.status === 200), 'production stylesheet not served');
    assert(responses.some(response => /javascript/.test(response.mime) && response.status === 200), 'production script not served');
    await privacy();
    await scenario({client, first, received, mutations, responses, cdp, evaluate, click, tab, select, reload, navigate, privacy, inertBody, capture, startClient, buttonExpression});
    await privacy();
  } finally {
    if (ws) ws.close();
    await Promise.all(children.map(process => stopChild(process, process.spawnfile === clientBin ? 'SIGINT' : 'SIGTERM')));
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    fs.rmSync(root, {recursive: true, force: true});
  }
}

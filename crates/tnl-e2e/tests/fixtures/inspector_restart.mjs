import assert from 'node:assert/strict';
import {stopChild} from './browser_process.mjs';
import {withInspector, until, delay} from './inspector_browser.mjs';

await withInspector(async fixture => {
  const {client, first, received, mutations, responses, cdp, evaluate, click, tab, reload, navigate, startClient, buttonExpression} = fixture;
  await tab('Replay'); await click('Check replay availability');
  await until(() => evaluate(`${buttonExpression('Prepare replay review')}?.disabled === false`), 'replay availability missing');
  await click('Prepare replay review');
  await until(() => evaluate('Boolean(document.querySelector(".replay-ack input"))'), 'review missing');
  await evaluate('document.querySelector(".replay-ack input").click()');
  assert.equal(await evaluate(`${buttonExpression('Confirm replay')}.disabled`), false);
  // Retain the detached node only to verify that expired UI removes its confirmation controls and emits no POST.
  await evaluate(`void (globalThis.staleConfirmButton = ${buttonExpression('Confirm replay')})`);
  assert.equal(received.length, 1); assert.equal(mutations.length, 1);
  await stopChild(client.cli.process, 'SIGINT');
  const fresh = await startClient(Number(client.url.port));
  assert.equal(fresh.url.origin, client.url.origin, 'restart must bind the SAME inspector port');
  assert.notEqual(fresh.cap, client.cap, 'new CLI run must rotate capability');
  assert.equal((await fresh.api('/api/v1/stats', client.cap)).status, 401, 'old capability still authenticates');
  assert.equal((await fresh.api('/api/v1/stats')).status, 200, 'fresh capability refused');
  const listResponse = await fresh.api('/api/v1/requests?limit=50');
  assert.equal(listResponse.status, 200); assert.deepEqual((await listResponse.json()).items, [], 'restart restored history');
  assert.equal((await fresh.api('/api/v1/requests/' + first.record.capture_id)).status, 404, 'old capture survived restart');
  await until(() => evaluate('document.body.innerText.includes("Access expired")'), 'stale UI did not report expired capability');
  assert(responses.some(response => response.pathname.startsWith('/api/v1/') && response.status === 401), 'stale browser did not receive real 401');
  assert.equal(await evaluate('Boolean(document.querySelector(".replay-review, .replay-ack input:checked"))'), false, 'stale UI retained acknowledgment');
  await evaluate('globalThis.staleConfirmButton.click(); globalThis.staleConfirmButton.click(); delete globalThis.staleConfirmButton');
  await delay(2200);
  assert.equal(mutations.length, 1, 'expired UI emitted a confirmation POST');
  assert.equal(received.length, 1, 'restart or stale UI replayed automatically');
  await reload();
  await until(() => evaluate('document.body.innerText.includes("Access expired")'), 'old fragment reload must remain refused');
  assert.equal(await evaluate('Boolean(document.querySelector(".replay-review"))'), false);
  // Reopen the printed link as a fresh document, not a same-document hash edit.
  await cdp('Page.navigate', {url: 'about:blank'});
  await until(() => evaluate('location.href === "about:blank"'), 'old document did not close');
  await navigate(fresh.link);
  await until(() => evaluate('document.body.innerText.includes("No matching captures retained")'), 'fresh UI history not empty');
  assert.equal(await evaluate('new URLSearchParams(location.hash.slice(1)).get("token")'), fresh.cap);
  assert.equal(await evaluate('new URLSearchParams(location.hash.slice(1)).has("theme")'), false, 'fresh startup should default to Auto');
  assert.equal(await evaluate('Boolean(document.querySelector(".replay-review, .replay-ack input:checked"))'), false);
  await delay(2200);
  assert.equal(mutations.length, 1, 'restart/reload must never prepare/confirm automatically');
  assert.equal(received.length, 1);
  console.log(JSON.stringify({scenario: 'restart', same_inspector_port: true, fresh_capability: true, old_capability_status: 401, fresh_capability_status: 200, old_capture_status: 404, fresh_history_records: 0, expired_UI_removed_confirmation_controls_and_emitted_no_POST: true, restart_reload_restored_authority: false, POST_operations: ['prepare'], backend_requests: received.length}));
});

import assert from 'node:assert/strict';
import {withInspector, until, delay} from './inspector_browser.mjs';

await withInspector(async fixture => {
  const {client, first, received, mutations, responses, cdp, evaluate, click, tab, select, reload, privacy, inertBody, buttonExpression} = fixture;
  await inertBody();
  await tab('Response'); await inertBody();
  await tab('Replay');
  await click('Check replay availability');
  await until(() => evaluate(`${buttonExpression('Prepare replay review')}?.disabled === false`), 'capture not eligible for replay');
  await click('Prepare replay review');
  await until(() => evaluate('Boolean(document.querySelector(".replay-review"))'), 'explicit prepare did not produce a review');
  assert.equal(received.length, 1, 'prepare must send zero backend requests');
  assert.equal(await evaluate(`${buttonExpression('Confirm replay')}.disabled`), true);
  await privacy();
  const accessibility = await cdp('Accessibility.getFullAXTree');
  assert(accessibility.nodes.some(node => !node.ignored && node.role?.value === 'checkbox' && node.name?.value === 'I understand this can cause side effects'), 'replay acknowledgment must have an accessible label');
  await evaluate('document.querySelector(".replay-ack input").focus()');
  await cdp('Input.dispatchKeyEvent', {type: 'keyDown', key: ' ', code: 'Space', windowsVirtualKeyCode: 32});
  await cdp('Input.dispatchKeyEvent', {type: 'keyUp', key: ' ', code: 'Space', windowsVirtualKeyCode: 32});
  assert.equal(await evaluate('document.querySelector(".replay-ack input").checked'), true, 'keyboard acknowledgment did not toggle');
  assert.equal(await evaluate(`${buttonExpression('Confirm replay')}.disabled`), false);
  // Theme and remount preserve one live review, but cannot execute it.
  await evaluate(`(() => {const theme = document.querySelector('[aria-label="Theme"]'); theme.value = 'dark'; theme.dispatchEvent(new Event('change', {bubbles: true}));})()`);
  await tab('Request'); await tab('Replay');
  assert.equal(await evaluate('document.querySelector(".replay-ack input").checked'), true);
  await delay(2200);
  assert.equal(await evaluate('document.querySelector("[data-testid=capture-id]").textContent'), first.record.capture_id, 'polling lost selection');
  assert.equal(mutations.length, 1, 'theme/tab/remount/poll must not mutate');
  await reload();
  await until(() => evaluate('document.querySelectorAll("[data-capture-id]").length === 1'), 'authenticated F5 lost history');
  assert.equal(await evaluate('new URLSearchParams(location.hash.slice(1)).get("token")'), client.cap);
  assert.equal(await evaluate('new URLSearchParams(location.hash.slice(1)).get("theme")'), 'dark');
  assert.equal(await evaluate('document.documentElement.dataset.theme'), 'dark');
  assert.equal(await evaluate('Boolean(document.querySelector(".replay-ack input:checked"))'), false);
  assert.equal(received.length, 1); assert.equal(mutations.length, 1, 'F5 must not prepare or confirm');
  await select(first.record.capture_id); await tab('Replay');
  assert.equal(await evaluate('Boolean(document.querySelector(".replay-review"))'), false, 'F5 restored confirmation authority');
  assert.equal(await evaluate(`Boolean(${buttonExpression('Confirm replay')})`), false);
  assert.equal(await evaluate(`${buttonExpression('Prepare replay review')}.disabled`), true, 'fresh availability check required');
  await click('Check replay availability');
  await until(() => evaluate(`${buttonExpression('Prepare replay review')}?.disabled === false`), 'fresh availability missing');
  await click('Prepare replay review');
  await until(() => evaluate('Boolean(document.querySelector(".replay-ack input"))'), 'fresh explicit review missing');
  assert.equal(await evaluate('document.querySelector(".replay-ack input").checked'), false, 'fresh acknowledgment required');
  assert.equal(await evaluate(`${buttonExpression('Confirm replay')}.disabled`), true);
  assert.equal(received.length, 1);
  await evaluate('document.querySelector(".replay-ack input").click()');
  // Hold the same node and dispatch twice in one JS turn (before React can repaint).
  await evaluate(`(() => {const confirm = ${buttonExpression('Confirm replay')}; confirm.click(); confirm.click();})()`);
  await until(() => received.length === 2, 'confirmed replay did not arrive');
  await until(async () => {
    const response = await client.api('/api/v1/requests/' + first.record.capture_id);
    return response.ok && (await response.json()).last_replay_outcome?.delivery === 'response_received';
  }, 'server replay outcome missing');
  await click('Check replay outcome');
  await until(() => evaluate('document.querySelector(".replay-outcome")?.textContent.includes("response_received")'), 'linked outcome missing');
  assert.equal(received[1].method, received[0].method);
  assert.equal(received[1].url, received[0].url);
  assert.deepEqual(received[1].body, received[0].body, 'replay changed original body bytes');
  assert(received[0].wire.length > received[0].body.length, 'wire capture must include request head');
  assert.deepEqual(received[1].wire, received[0].wire, 'replay changed the original HTTP wire bytes');
  for (const header of ['authorization', 'cookie', 'host', 'content-type', 'content-length']) assert.equal(received[1].headers[header], received[0].headers[header]);
  await tab('Metadata'); await tab('Replay');
  await delay(2300);
  const detailResponse = await client.api('/api/v1/requests/' + first.record.capture_id);
  assert.equal(detailResponse.status, 200);
  const detail = await detailResponse.json();
  assert.equal(detail.last_replay_outcome.parent_capture_id, first.record.capture_id);
  assert.equal(detail.last_replay_outcome.state, 'complete');
  assert.equal(detail.last_replay_outcome.delivery, 'response_received');
  assert.equal(detail.last_replay_outcome.tunnel_generation, first.record.generation);
  await reload();
  await until(() => evaluate('document.querySelectorAll("[data-capture-id]").length >= 1'), 'history missing after outcome F5');
  await select(first.record.capture_id); await tab('Replay');
  await until(() => evaluate('document.querySelector(".replay-outcome")?.textContent.includes("Last retained replay outcome (read-only)")'), 'retained linked result missing');
  assert.equal(await evaluate('Boolean(document.querySelector(".replay-review, .replay-ack input:checked"))'), false);
  assert.equal(received.length, 2, 'poll/theme/remount/reload or duplicate click replayed automatically');
  assert.deepEqual(mutations, [
    `/api/v1/requests/${first.record.capture_id}/replay/prepare`,
    `/api/v1/requests/${first.record.capture_id}/replay/prepare`,
    `/api/v1/requests/${first.record.capture_id}/replay/confirm`,
  ]);
  const posts = responses.filter(response => response.method === 'POST').map(response => ({operation: response.pathname.split('/').at(-1), status: response.status}));
  assert.deepEqual(posts.map(response => response.operation), ['prepare', 'prepare', 'confirm']);
  assert(posts.every(response => response.status >= 200 && response.status < 300));
  await privacy();
  console.log(JSON.stringify({scenario: 'replay', production_assets: true, public_backend_API_UI_ID_correlation: true, redaction_and_inert_captured_HTML_SVG: true, prepare_backend_requests: 0, fresh_ack_after_F5: true, same_turn_double_confirm_executions: 1, original_HTTP_wire_method_query_auth_cookie_host_body_bytes: true, retained_linked_outcome: true, no_automatic_mutations: true, POST_responses: posts, backend_requests: received.length}));
});

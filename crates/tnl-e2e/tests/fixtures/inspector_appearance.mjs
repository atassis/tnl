import assert from 'node:assert/strict';
import {withInspector, until, delay} from './inspector_browser.mjs';

await withInspector(async fixture => {
  const {client, first, received, mutations, cdp, evaluate, click, tab, select, reload, capture, inertBody} = fixture;
  async function systemTheme(value) {
    await cdp('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value}]});
  }
  async function theme(value) {
    await evaluate(`(() => {const control = document.querySelector('[aria-label="Theme"]'); control.value = ${JSON.stringify(value)}; control.dispatchEvent(new Event('change', {bubbles: true}));})()`);
  }
  async function resolved(value) {
    await until(() => evaluate(`document.documentElement.dataset.theme === '${value}'`), 'system/explicit theme did not resolve');
    assert.equal(await evaluate('getComputedStyle(document.documentElement).colorScheme'), value, 'live stylesheet theme not applied');
  }
  await systemTheme('dark'); await resolved('dark');
  const darkBackground = await evaluate('getComputedStyle(document.body).backgroundColor');
  await systemTheme('light'); await resolved('light');
  assert.notEqual(await evaluate('getComputedStyle(document.body).backgroundColor'), darkBackground, 'Auto did not change computed colors');
  await theme('light'); await systemTheme('dark'); await resolved('light');
  await tab('Response');
  const second = await capture('/wrap/' + 'longsegment'.repeat(24) + '?secret=RAW_QUERY_CANARY');
  await until(() => evaluate('document.querySelectorAll("[data-capture-id]").length === 2'), 'new capture not polled');
  assert.equal(await evaluate('document.querySelector("[data-testid=capture-id]").textContent'), first.record.capture_id, 'background update reset selection');
  assert.equal(await evaluate('document.querySelector("[role=tab][aria-selected=true]").textContent'), 'Response', 'theme/poll reset tab');
  await reload();
  await until(() => evaluate('document.querySelectorAll("[data-capture-id]").length === 2'), 'F5 lost authenticated history');
  assert.equal(await evaluate('new URLSearchParams(location.hash.slice(1)).get("token")'), client.cap);
  assert.equal(await evaluate('new URLSearchParams(location.hash.slice(1)).get("theme")'), 'light');
  await resolved('light');
  await select(second.record.capture_id);
  await theme('auto'); await resolved('dark');
  assert.equal(await evaluate('new URLSearchParams(location.hash.slice(1)).has("theme")'), false);
  assert.equal(await evaluate('new URLSearchParams(location.hash.slice(1)).get("token")'), client.cap);
  await inertBody();
  for (const width of [1440, 390]) {
    await cdp('Emulation.setDeviceMetricsOverride', {width, height: 1000, deviceScaleFactor: 1, mobile: false});
    await delay(80);
    const dimensions = await evaluate(`(() => {
      const selectors = ['html', '.app', '.workspace', '.detail-pane', '.detail-heading', '.detail-content', '[data-testid=captured-body]', '.timing-table'];
      return selectors.map(selector => {const node = document.querySelector(selector), rect = node.getBoundingClientRect(); return {selector, client: node.clientWidth, scroll: node.scrollWidth, left: rect.left, right: rect.right};});
    })()`);
    for (const item of dimensions) {
      assert(item.scroll <= item.client + 1, `${width}px horizontal overflow in ${item.selector}: ${item.scroll}/${item.client}`);
      assert(item.left >= -1 && item.right <= width + 1, `${width}px element outside viewport: ${item.selector}`);
    }
    assert.equal(await evaluate('getComputedStyle(document.querySelector("[data-testid=captured-body]")).whiteSpace'), 'pre-wrap');
    assert.equal(await evaluate('getComputedStyle(document.querySelector(".detail-heading h2")).overflowWrap'), 'anywhere');
    assert.equal(await evaluate('document.querySelector("[data-testid=capture-id]").textContent'), second.record.capture_id);
  }
  assert.equal(await evaluate('getComputedStyle(document.querySelector(".workspace")).display'), 'flex', '390px responsive stylesheet not active');
  assert.equal(await evaluate(`document.querySelectorAll('svg[aria-label="Client-to-backend timing"]').length`), 1, 'known app SVG timeline allowed');
  assert(await evaluate('document.querySelector("[aria-label=Timing]").textContent.includes("Intervals overlap; do not sum them")'));
  assert(await evaluate(`document.querySelector('[aria-label="Timing intervals"]').textContent.includes('completed')`), 'timing state must be textual');
  assert(await evaluate('document.querySelector(".milestones").textContent.includes("First backend byte · from entry")'), 'timing origin missing');
  assert(await evaluate('document.querySelector(".timing-table").textContent.includes("Observed duration")'));

  // Native keyboard navigation, not synthetic React handler invocation.
  await evaluate('document.querySelector("[aria-label=Theme]").focus()');
  await cdp('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Tab', code: 'Tab', windowsVirtualKeyCode: 9});
  await cdp('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Tab', code: 'Tab', windowsVirtualKeyCode: 9});
  assert.equal(await evaluate('document.activeElement.textContent'), 'Pause updates');
  assert.equal(await evaluate('getComputedStyle(document.activeElement).outlineStyle'), 'solid', 'keyboard focus ring missing');
  await evaluate('document.querySelector("[role=tab][aria-selected=true]").focus()');
  await cdp('Input.dispatchKeyEvent', {type: 'keyDown', key: 'ArrowRight', code: 'ArrowRight', windowsVirtualKeyCode: 39});
  await cdp('Input.dispatchKeyEvent', {type: 'keyUp', key: 'ArrowRight', code: 'ArrowRight', windowsVirtualKeyCode: 39});
  assert.equal(await evaluate('document.activeElement.textContent'), 'Response');
  assert.equal(await evaluate('document.querySelector("[role=tabpanel]").getAttribute("aria-labelledby")'), 'detail-tab-Response');
  await inertBody();
  const ax = await cdp('Accessibility.getFullAXTree');
  const names = ax.nodes.filter(node => !node.ignored).map(node => node.name?.value);
  for (const name of ['Theme', 'Search', 'Pause updates', 'Capture details', 'Response', 'Timing intervals']) assert(names.includes(name), 'accessible control/table missing: ' + name);
  await tab('Replay'); await click('Check replay availability');
  await until(() => evaluate('document.querySelector(".replay-panel").textContent.includes("eligible")'), 'replay availability missing');
  // No app-generated violations before deliberate policy probes.
  assert.deepEqual(await evaluate('globalThis.cspViolations'), [], 'production UI violates native CSP');
  await evaluate(`(() => {const script = document.createElement('script'); script.textContent = 'globalThis.CSP_SCRIPT_EXECUTED = true'; document.head.append(script); const style = document.createElement('style'); style.textContent = 'body { --csp-probe: violated; }'; document.head.append(style);})()`);
  await until(() => evaluate('globalThis.cspViolations.length === 2'), 'native CSP did not reject inline script/style');
  const violations = await evaluate('globalThis.cspViolations');
  assert.deepEqual(violations.map(value => value.directive).sort(), ['script-src-elem', 'style-src-elem']);
  assert(violations.every(value => value.blocked === 'inline'));
  assert.equal(await evaluate('typeof globalThis.CSP_SCRIPT_EXECUTED'), 'undefined');
  assert.equal(await evaluate('getComputedStyle(document.body).getPropertyValue("--csp-probe")'), '');
  assert.equal(mutations.length, 0, 'appearance, tabs, polling, reload or safe reads must not POST');
  assert.equal(received.length, 2, 'appearance checks must not replay');
  console.log(JSON.stringify({scenario: 'appearance', system_Auto_and_explicit_override_computed_styles: true, F5_capability_theme_retained: true, stable_selected_capture_and_tabs: true, viewports: [1440, 390], wrapped_body_and_target_no_horizontal_overflow: true, native_keyboard_and_accessible_names: true, textual_timing_and_app_SVG: true, application_CSP_violations: 0, native_blocked_probes: violations, POST_count: mutations.length, backend_requests: received.length}));
});

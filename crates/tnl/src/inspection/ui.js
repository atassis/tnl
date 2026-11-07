"use strict";
(() => {
  let token = new URLSearchParams(location.hash.slice(1)).get("token");
  let before = null;
  let selectedCapture = null;
  let selectedReplay = null;
  let pendingTicket = null;
  const status = document.getElementById("status");
  const rows = document.getElementById("rows");
  const detail = document.getElementById("detail");
  const review = document.getElementById("replay-review");
  const previewText = document.getElementById("replay-preview");
  const acknowledge = document.getElementById("replay-ack");
  const confirmButton = document.getElementById("replay-confirm");
  const outcomeText = document.getElementById("replay-outcome");
  const parentButton = document.getElementById("replay-parent");
  async function api(path, body) {
    if (!token) throw new Error("Reopen the private startup link to authenticate.");
    const headers = {Authorization: "Bearer " + token};
    const options = {headers, cache: "no-store", credentials: "omit", redirect: "error"};
    if (body !== undefined) { headers["Content-Type"] = "application/json"; options.method = "POST"; options.body = JSON.stringify(body); }
    const response = await fetch(path, options);
    if (!response.ok) throw new Error("Inspector request failed (" + response.status + "). No automatic retry.");
    return response.json();
  }
  function replayPath(id) { return "/api/v1/requests/" + encodeURIComponent(id) + "/replay"; }
  function clearTicket() {
    pendingTicket = null;
    acknowledge.checked = false;
    confirmButton.disabled = true;
    previewText.textContent = "";
  }
  async function showDetail(id) {
    selectedCapture = id;
    clearTicket();
    review.hidden = true;
    document.getElementById("replay-availability").textContent = "Checking replay eligibility…";
    try {
      const record = await api("/api/v1/requests/" + encodeURIComponent(id));
      if (selectedCapture !== id) return;
      detail.textContent = JSON.stringify(record, null, 2);
      const data = await api(replayPath(id));
      if (selectedCapture !== id) return;
      document.getElementById("replay-availability").textContent = "Replay " + data.availability.state + (data.availability.reason ? ": " + data.availability.reason : "");
      review.hidden = data.availability.state !== "eligible";
    } catch (error) { if (selectedCapture === id) document.getElementById("replay-availability").textContent = error.message; }
  }
  async function refreshOutcome() {
    const id = selectedReplay;
    if (!id) return;
    try {
      const data = await api("/api/v1/replays/" + encodeURIComponent(id));
      if (selectedReplay !== id) return;
      outcomeText.textContent = data.outcome ? JSON.stringify(data.outcome, null, 2) : "Delivery unknown: outcome missing or expired. Do not automatically resubmit.";
    } catch (error) { if (selectedReplay === id) outcomeText.textContent = "Delivery unknown: " + error.message; }
  }
  review.addEventListener("click", async () => {
    const id = selectedCapture;
    clearTicket();
    review.disabled = true;
    try {
      const data = await api(replayPath(id) + "/prepare", {schema_version: 1});
      if (selectedCapture !== id) return;
      // The ticket stays only in this closure, never in DOM, storage or history.
      pendingTicket = {captureId: id, replayId: data.preview.replay_id, nonce: data.preview.confirmation_token};
      previewText.textContent = "Review ONE attempt (prepare sent nothing):\nMethod: " + data.method + "\nSanitized path: " + data.request_target + "\nFixed original backend: " + data.preview.target + "\nHost policy: " + data.preview.host_policy + "\nOriginal body bytes: " + data.preview.request_body_bytes + "\nConfirmation expires: " + new Date(data.preview.confirmation_expires_at_unix_ms).toISOString() + "\nRisk: original secrets and unedited request are reused; side effects and expired signatures are possible. No redirects, automatic retry, edits or destination overrides.";
    } catch (error) { status.textContent = error.message; }
    finally { review.disabled = false; }
  });
  acknowledge.addEventListener("change", () => { confirmButton.disabled = !pendingTicket || !acknowledge.checked; });
  confirmButton.addEventListener("click", async () => {
    if (!pendingTicket || !acknowledge.checked) return;
    const ticket = pendingTicket;
    clearTicket(); // Invalidate synchronously before awaiting: repeated clicks cannot submit.
    selectedReplay = ticket.replayId;
    parentButton.hidden = false;
    parentButton.onclick = () => showDetail(ticket.captureId);
    outcomeText.textContent = "Delivery unknown while submitting. Do not resubmit automatically.";
    try {
      const data = await api(replayPath(ticket.captureId) + "/confirm", {schema_version: 1, replay_id: ticket.replayId, confirmation_token: ticket.nonce, confirm_side_effects: true});
      if (selectedReplay === ticket.replayId) outcomeText.textContent = data.outcome ? JSON.stringify(data.outcome, null, 2) : "Delivery unknown: no outcome returned.";
    } catch (error) { outcomeText.textContent = "Delivery unknown after submission: " + error.message; }
    // Only safe GET is used for subsequent outcome polling. A new attempt
    // requires another explicit review and acknowledgement, never a retry loop.
    await refreshOutcome();
  });
  async function load(older) {
    try {
      const query = new URLSearchParams({q: document.getElementById("query").value, limit: "50"});
      if (older && before !== null) query.set("before", String(before));
      const data = await api("/api/v1/requests?" + query);
      rows.replaceChildren();
      for (const record of data.items) {
        const row = document.createElement("tr");
        for (const value of [record.method, record.request_target, record.status ?? "—", record.state, record.connection]) {
          const cell = document.createElement("td"); cell.textContent = String(value); row.appendChild(cell);
        }
        const cell = document.createElement("td"); const button = document.createElement("button"); button.textContent = "Details"; button.type = "button";
        button.addEventListener("click", () => showDetail(record.capture_id));
        cell.appendChild(button); row.appendChild(cell); rows.appendChild(row);
      }
      before = data.next_before;
      const stats = await api("/api/v1/stats");
      status.textContent = stats.records + " retained records; tunnel " + (stats.tunnel.connected ? "connected" : "disconnected");
    } catch (error) { status.textContent = error.message; }
  }
  document.getElementById("search").addEventListener("click", () => load(false));
  document.getElementById("more").addEventListener("click", () => load(true));
  if (token) { load(false); setInterval(() => { if (!document.hidden) { load(false); refreshOutcome(); } }, 2000); }
})();

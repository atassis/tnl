"use strict";
(() => {
  let token = new URLSearchParams(location.hash.slice(1)).get("token");
  history.replaceState(null, "", location.pathname);
  let before = null;
  const status = document.getElementById("status");
  const rows = document.getElementById("rows");
  const detail = document.getElementById("detail");
  async function api(path) {
    if (!token) throw new Error("Reopen the private startup link to authenticate.");
    const response = await fetch(path, {headers: {Authorization: "Bearer " + token}, cache: "no-store", credentials: "omit", redirect: "error"});
    if (!response.ok) throw new Error("Inspector request failed (" + response.status + ").");
    return response.json();
  }
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
        const cell = document.createElement("td"); const button = document.createElement("button"); button.textContent = "Details";
        button.addEventListener("click", async () => { try { detail.textContent = JSON.stringify(await api("/api/v1/requests/" + encodeURIComponent(record.capture_id)), null, 2); } catch (error) { status.textContent = error.message; } });
        cell.appendChild(button); row.appendChild(cell); rows.appendChild(row);
      }
      before = data.next_before;
      const stats = await api("/api/v1/stats");
      status.textContent = stats.records + " retained records; tunnel " + (stats.tunnel.connected ? "connected" : "disconnected");
    } catch (error) { status.textContent = error.message; }
  }
  document.getElementById("search").addEventListener("click", () => load(false));
  document.getElementById("more").addEventListener("click", () => load(true));
  if (token) { load(false); setInterval(() => { if (!document.hidden) load(false); }, 2000); }
})();

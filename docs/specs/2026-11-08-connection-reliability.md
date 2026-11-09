# Connection reliability

Approved direction: simple client-owned recovery, one beta protocol, bounded
failed-session termination, and connected-client remote stop. The previous
durable-stop proposal is superseded: no run grants, database, state directory,
authentication overhaul or durable stop guarantee.

User approved this direction after its limitations were explained. Initial
budgets below are engineering starting values, not measured guarantees.

## 1. Connection lifecycle

Keep the existing in-memory registry and create/reattach model:

```text
connect → register → serve + heartbeat
                    ↓ unexpected loss
              disconnect → backoff
                    ↓
        reconnect → reattach previous tunnel
                    ↓ TunnelLost only
        recreate from retained client settings
```

- Within grace: preserve tunnel identity, protected admission state and inspection generation.
- After grace or daemon restart: create a new tunnel using retained settings and the previously assigned name.
- Preserve target, Host behavior, public/protected mode and exact protected policy.
- Preserve completed local history; revoke replay authority immediately on disconnection. A new routing tunnel gets the appropriate new inspection generation.
- Never replay an interrupted request or migrate an existing HTTP/WS stream.
- Never take over an active owner. Reattach encountering the old active session returns a typed temporary error and retries while its lease expires.
- Genuine name collision does not authorize takeover or silent random-name substitution.

No server-side persistence, durable name ownership or new authentication model is introduced.

## 2. Unified protocol

Add one exact control protocol header/version, for example:

```text
x-tnl-control-protocol: heartbeat-v1
```

The server checks it before upgrade/session registration; the client validates the echo before sending registration messages.

- Missing/wrong version: clear terminal incompatibility.
- No capability-negotiation state machine or fallback mode.
- Retain existing metadata/policy validation; do not remove unrelated safety checks.

Keep current create/reattach messages. Add:

```text
Heartbeat    { tunnel_id, sequence }
HeartbeatAck { tunnel_id, sequence }

ErrorCode::TunnelAlreadyActive

Closing { reason: ClientShutdown | RemoteStop }
```

Use typed server errors for public and protected reconnect classification. In particular:

- `TunnelAlreadyActive`: retryable in both modes.
- `TunnelLost`: permits same-policy recreation.
- Authentication, protocol and policy failures: terminal.
- A validated `RemoteStop`: terminal success/intentional termination, **never recreate**.

## 3. Remote stop: heartbeat response is sufficient

The current admin handler only removes registry entries; it neither notifies nor closes the session. A push-notification channel is **not necessary** for this delivery.

Minimal implementation:

1. Successful registration records the bound tunnel ID in that control loop.
2. Each heartbeat validates that this registered session still owns that active tunnel.
3. If the tunnel was removed while that same registered control session remains valid, reply `Closing(RemoteStop)`.
4. The client receives that response through its sole control owner, revokes replay authority, stops accepting/forwarding and exits.
5. Do not close the control connection immediately from the admin handler: an EOF alone is recoverable and would hide the reason.

The admin operation keeps its existing authorization and reusable-name interface. No new run-management API is needed.

**Ownership safety:**

- Check the original tunnel ID and session ID, not just its hostname.
- Registry removal remains atomic under the lifecycle lock.
- A stopped name reused by another tunnel does not let the old heartbeat acknowledge or modify that newer tunnel.
- Expired/removed sessions and superseded ownership are transport/lifecycle failure, not an invented remote-stop notification.
- Existing session-scoped late cleanup/Close must not remove a newer binding.

**Honest guarantee:** on a healthy control path, the client learns stop at its next heartbeat exchange. If that indication is not delivered before disconnection/restart, automatic recreation remains possible. Once decoded, it must win over recoverable EOF handling.

## 4. One control owner and framing-safe shutdown

Exactly one task/future owns control reads and exchanges.

- At most one heartbeat is outstanding.
- Validate sequence and tunnel identity.
- Share the owner with intentional shutdown; never call `close_control` concurrently with a heartbeat read.
- Preserve the pending decoder/read across a shutdown transition, or abandon the entire stream.
- Never restart length-prefix decoding after cancelling a partially consumed frame.
- Shutdown may finish the outstanding exchange and send Close within its cleanup budget; otherwise report unacknowledged release.
- Prefer an already available validated stop result over a simultaneous accept-loop termination.
- Keep bounded frame allocation and sanitized errors; no token/policy/message-body logging.

A remote stop is distinct from “binding absent on a newly connected daemon”. Only the former ends the client run.

## 5. Liveness, deadlines and retry

Initial engineering budgets:

| Operation | Budget |
|---|---:|
| Heartbeat interval | 15 s |
| Heartbeat write/flush/read exchange | 5 s |
| Server silence lease | 45 s |
| Whole connection/registration attempt | 20 s |
| Data-stream opening, including mutex wait | 5 s |
| Failed-session cleanup before forcing failure | 2 s |

- Use one overall client attempt deadline, including reattach→create fallback—not a fresh unlimited budget for every sub-operation.
- Bound server control-stream bootstrap/initial registration as well.
- Renew the lease only after a complete valid control message. Partial bytes do not keep a session alive indefinitely.
- Bound server response writing as well as reading.
- On expiry, invalidate the session and owner-fenced routing before cleanup.
- Add bounded jitter to existing exponential backoff. Avoid repeatedly resetting it after immediately failing registrations.
- Do not change backend body, upload, SSE or WS lifetime timeout contracts.

Do not promise timer progress while the laptop is suspended. Qualify recovery after wake/network return.

## 6. Ownership and failed-session termination

Keep run-owned tracking of accepted forwards; add explicit **per-generation accept ownership**.

- Stop and join the old generation’s accept task instead of detaching its `JoinHandle`.
- Previously accepted forwards remain tracked and are not dropped merely because a new connection starts.
- On the approved forced-failure path, terminate resources belonging to that conclusively unhealthy physical session. If outstanding forward futures need cancellation to release backend IO, scope that cancellation to the failed generation—not every generation.
- Explicit CLI/remote stop still terminates all run-owned work.

Transport changes must distinguish:

- **Healthy graceful close/drop:** continue `poll_close`, preserving queued writes.
- **Explicit failed-session termination:** allow two seconds of cleanup, then signal the driver to drop the failed connection.

Do not implement this with `JoinHandle::abort()` or a blanket timeout on every graceful close. The force signal must remain observable while `poll_close` itself is blocked.

Own cancellation/completion of the daemon’s WS bridge as well; dropping the yamux side alone is not sufficient proof of bounded bridge termination.

## 7. Bounded, cancellation-aware stream opening

A timeout around the current `open_stream()` alone is insufficient: its unbounded queue can retain cancelled requesters.

- Bound pending outbound requests and inbound stream delivery.
- Remove cancelled requesters before opening, and wake the driver when a pending requester is cancelled.
- Preserve simultaneous outbound/inbound polling so yamux ACKs continue progressing.
- Do not block the driver on a full inbound send.
- Bound work drained per driver turn.
- Include session-mutex acquisition in the data-open deadline.
- Invalidate stale handle clones and recheck the binding after asynchronous opening, before admitting metadata/application bytes.
- An individual opening timeout does not delete the tunnel or replay that visitor request.

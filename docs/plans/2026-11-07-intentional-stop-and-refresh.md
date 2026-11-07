# Intentional stop and inspector refresh

## Agreed behavior

Ctrl+C and SIGTERM stop immediately rather than draining active HTTP/WebSocket
connections. Reconnect must stop; the client attempts bounded, acknowledged
release of its own session's tunnel. Only an explicit server acknowledgment
permits reporting release. Unexpected transport loss keeps the existing grace
and same-policy reattach/recreation semantics. Never delete a tunnel by hostname
as a cleanup fallback: an old shutdown must not remove a newer binding.

The existing Close control message denotes intentional session termination.
The daemon removes only bindings still owned by that session before returning
Closing. EOF/error retains the configured grace. New clients tolerate missing
acknowledgment from older/unreachable servers by warning, not assuming release.
Signal cancellation during registration or reconnect must be bounded and must
not leave a detached registration that later creates a tunnel.

Forwarding tasks remain alive across ordinary reattach/recreation where needed
by existing exact-drain contracts. Explicit shutdown instead cancels and joins
run-owned accept/forwarding tasks, without aborting the yamux driver itself.

The private inspector capability remains in the URL fragment, as selected by
the user. No cookies, localStorage or sessionStorage. Refresh retains API
access, not replay confirmation/acknowledgment or automatic resubmission. The
link is a private bearer capability; it may remain in browser history/session
restore while the CLI lives. A new CLI process issues a new capability.

## Implementation and verification

1. Observe RED for Close acknowledgment/release with nonzero grace and real
   binary signal -> immediate same-name restart; preserve EOF grace tests.
2. Implement session-scoped Close/Closing and bounded client shutdown on both
   signals, covering startup, active I/O and retry/backoff. Preserve public and
   protected immutable options; track/cancel/join owned tasks only at shutdown.
3. Observe RED for retained fragment, then update UI and actual isolated browser
   probe: F5 authenticated history; no storage/cookies; no replay after refresh.
4. Test stale old-session Close after reattach, unavailable/missing-ack server,
   Ctrl+C/SIGTERM, named/random tunnels, inspector on/off, active HTTP/WS and
   unchanged unexpected-loss grace/recreation. Run full fmt/clippy/workspace.

No UI redesign, changed administrative stop semantics, conflict retry policy,
protocol replacement, deployment, global client replacement or publication is
part of this patch. Those need separate reconciliation/authorization.


## Current implementation evidence

Server Close removes current session bindings before Closing(client_shutdown).
Run-owned TaskTracker/CancellationToken state spans reconnect generations, so
normal network loss still permits old exact-drain contracts; explicit stop
cancels active forwarding, revokes inspection/replay authority before waiting
for network cleanup, then cancels and joins accept/forward tasks. No yamux driver
abort, hostname-based deletion fallback or public-policy downgrade was added.

Observed RED: original Close returned EOF instead of ACK; real Ctrl+C left a
name reserved with 3600-second grace; original inspector JS cleared fragment.
A new slow-ACK test also caught delayed inspection/replay deactivation during
shutdown; it was fixed before final acceptance. Browser baseline beta.5 failed
the retained-fragment regression. Updated actual Brave passes F5, zero browser
storage/cookies, cleared replay acknowledgment/ticket state and single delivery
only after another explicit preparation/confirmation.

The complete configured debug-binary workspace, including every environment-
dependent ignored case, passed 446 tests with zero failed/ignored before the
final beta.6 version preparation. Two obsolete beta.1 image tests still internally
SKIP; these are not new beta.6 container/release qualification. Final commands
and totals are recorded in the completion report, not inferred from this prior
checkpoint. Production and the installed global CLI remain beta.5 until a new
rollout/install is explicitly approved.

Follow-up lifecycle decisions, not implemented here: meaningful fatal/retry
classification for initial SubdomainTaken/InvalidSubdomain; administrative stop
notification and compatibility with older clients; second-signal force exit;
and richer disconnected/auth-expired inspector states. Real network loss and
SIGKILL cannot guarantee immediate name release. A late or missing server ACK
must never be presented as successful release.


Final beta.6-source check: fmt, all-target locked clippy and complete configured
locked/no-fail-fast workspace pass: 447 reported passed, zero failed, zero
ignored. Explicit binaries are newly built debug beta.6; actual Caddy and Brave
are included. This is not frozen-release/image or deployed beta.6 acceptance.
The two-second deadline bounds protocol cleanup; it is not a hard-kill guarantee
for arbitrary blocking OS resolver/runtime work. That and second-signal forcing
remain explicit lifecycle follow-ups rather than untested guarantees.

Intentional release also propagates registry lock failure rather than silently
acknowledging removal. A poisoned-lifecycle regression and negative mutation
verify that false-success handling is rejected. Unexpected-disconnect cleanup
retains its preexisting public interface and grace semantics.

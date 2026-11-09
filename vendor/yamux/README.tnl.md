# Local yamux graceful-close fixes

This directory contains the published **yamux 0.13.10** source and normalized
manifest, selected by the workspace's `[patch.crates-io]`. It is deliberately
excluded from workspace membership; tnl's protocol regression tests exercise it
as a dependency. The workspace pins yamux to `=0.13.10` so a dependency update
cannot silently bypass this patch. No resolved runtime dependency versions were
changed.

## Provenance and license

- Upstream: <https://github.com/paritytech/yamux> (now redirects to
  <https://github.com/libp2p/rust-yamux>).
- Published archive: <https://static.crates.io/crates/yamux/yamux-0.13.10.crate>.
- Archive SHA-256:
  `1991f6690292030e31b0144d73f5e8368936c58e45e7068254f7138b23b00672`.
- Published VCS revision: `38e9944f8fbb723a3a4df575cfb15109efcb2d24`, subdirectory
  `yamux` (also recorded in `.cargo_vcs_info.json`).
- License: **Apache-2.0 OR MIT**. Original source copyright notices are retained.
  The archive omits the license texts; `LICENSE-APACHE` and `LICENSE-MIT` were
  copied from the upstream `yamux-v0.13.10` tag.
- `Cargo.toml` and `Cargo.toml.orig` are unchanged from the published archive.
  Its standalone `Cargo.lock` is omitted; this workspace's lockfile is authoritative.

## Local change

Only `src/connection.rs` differs from the published source.

### Wake local streams on close

`Active::close` calls the existing `drop_all_streams()` before transitioning into `Closing`.
This marks surviving local stream handles closed and wakes pending readers and
writers. It does **not** discard outgoing stream commands: `Closing` still drains
those commands and pending frames, sends termination, and flushes/closes the IO.

Without this transition, graceful local close drops the shared stream map without
updating its state or waking readers. A pending reader can hang, and a later read
can fail with `WriteZero` before draining already-buffered bytes because it tries
to send a window update over the closed command channel. Closed shared state lets
buffered reads drain before EOF, matching the existing remote/error cleanup path.

### Preserve a ready peer GoAway on write failure

`Active::poll` retains its healthy read/write order and backpressure. On write
readiness/flush failure only, it checks at most the next immediately-ready inbound
frame for an exact normal GoAway before returning the write error: GoAway tag,
zero flags, session stream ID zero, and normal reason zero. A peer can flush GoAway
and close its reader before our initial RTT ping is written; otherwise a healthy close
is incorrectly reported as BrokenPipe before its already-buffered GoAway is read.

This is not a blanket BrokenPipe-to-EOF mapping: no waiting, scanning through
other frames, or healthy-path read prioritization is introduced. If an earlier
inbound reply is pending, or the next read is pending, malformed, errored, or not
an exact normal GoAway, the original write IO error remains terminal. Protocol,
internal, or unknown GoAway reasons and malformed flags/stream IDs cannot replace
that confirmed write error. A decoder/read error already returned by normal
inbound polling is not revisited or replaced by a later frame.
Graceful `Closing` drain/flush is unchanged.

## Maintenance and removal

Public registry metadata and published sources were checked before patching:
0.13.10 is the latest 0.13 release; 0.14.0 and 0.14.1 retain the same faulty local
close path. The 0.14.1 split-stream waker fix is unrelated. Current upstream master
also retained this local-close behavior at the time of inspection.

When a compatible upstream release fixes both local stream wakeup and ready
GoAway/write-error ordering, remove the workspace patch, exclusion, exact version
pin, and this directory, update only yamux's lock
entry as needed, and rerun the protocol regressions and full workspace checks.
Do not replace this
fix with driver abort or early socket drop: those can lose queued writes.

Regression coverage lives beside the adapter in
`crates/tnl-protocol/src/transport.rs` under `yamux_tests`. Keep close/drop, both
roles, pending local IO, buffered reads, outgoing flush, and driver-resource
release coverage when updating or removing the patch. Also retain deterministic
ready-GoAway versus write readiness/flush failure, genuine malformed/read/write
failure with later GoAway, and simultaneous inbound/outbound backpressure tests.

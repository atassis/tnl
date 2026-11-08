# Local release evidence and retired worktrees

The beta.5 and beta.6 runtime artifacts and local acceptance records are retained
in a private archive, not committed as binaries or raw fixture logs. Logs and
browser profiles may contain synthetic credentials, private capability URLs and
machine-specific data; do not publish the raw archive.

Archive: `~/.local/share/tnl/archives/20261108T075748Z/` (owner-only).

- `release-candidates/45e0b7a/`: frozen beta.5 artifacts and original manifest.
- `release-candidates/cb340cb/`: frozen beta.6 artifacts, original manifest,
  cancelled initial campaign, successful recovery campaign, public acceptance,
  deployment receipt, and previous installed client backup.
- `worktrees/*.tar.zst`: original lane checkouts, including ignored evidence and
  the obsolete uncommitted WebSocket prototype; every regular file and symlink
  was verified against `inventory.json` before cleanup.
- `cache-evidence/`: non-build logs/scripts formerly mixed into Cargo targets.
- `integration-target/`: local integration logs; `tools/caddy`: verified fixture
  executable. `repository.bundle` preserves Git refs without relying on caches.

Original manifest hashes and recorded paths are unchanged. For paths referring
to the former release-validation worktree, use the corresponding relocated
`release-candidates/` path. The archive is a local recovery resource, not a
public release or remote backup.

## Accepted beta.6 provenance

Runtime artifacts were built from `cb340cb40d4ae4028fbb3e54a105c38a8f445c53`.
The repaired test harness is committed in
`ac730e8724fe2de4c507e977f6bb627a6956aae9`; its changes are tests/documentation
only. Do not describe the initially hung/cancelled campaign as passing.

- Qualified recovery: fmt/clippy, 447 reported workspace passes, zero failures
  and zero ignored, including the actual Brave refresh scenario.
- Two obsolete beta.1 container tests internally skip; they are not image proof.
  The actual beta.6 image independently passes 11 runtime checks; its extracted
  daemon passes six acceptance cases.
- Public verified-TLS acceptance covers immediate same-name Ctrl+C restart with
  inspection off/on, same-port inspector restart with token rotation and old-token
  refusal, SIGTERM acknowledgment, authenticated F5 without restored replay
  consent/resend, optional access policy, and application WebSockets.
- The approved daemon/client update is recorded in `deployment-receipt.json`.
  Caddy/gost were not restarted; configuration and unrelated inputs were retained.

## Cleanup rationale

All 11 retired lane HEADs are ancestors of `main`. Remaining uncommitted
WebSocket code is an earlier prototype of the integrated implementation, lacking
later identity/transport fixes and stronger assertions; applying it now would
regress current contracts. Preserve its original snapshot, not a reverse merge.
The two unique planning drafts are retained under `plans/archive/` and explicitly
marked historical. Keep the active `main` checkout and the original checkout's
unrelated local `AGENTS.md` edit. No branch deletion or Git publication is needed.

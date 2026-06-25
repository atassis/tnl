# Contributing to tnl

Thanks for your interest! Contributions are welcome.

## Before you start

- For anything more than a typo or an obvious bug fix, **open an issue first** to
  discuss the approach. The design lives in `docs/specs/`.
- tnl is a client/server system fronted by a reverse proxy. Most of it runs and
  tests locally with no domain or DNS (see `docs/RUNBOOK.md` §1).

## Development rules

The full development guide — build commands, the pre-commit quality gate, code
style, architecture, and the config model — lives in [`AGENTS.md`](AGENTS.md).
Please read it before opening a PR. The essentials:

- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` must all be clean before every commit.
- Conventional Commits (`type(scope): subject`); commit bodies explain *why*.
- No `unwrap()`/`expect()` on user-reachable paths.

## Pull requests

- Branch off `main`, keep the PR focused, describe what changed and why.
- Add or update tests for behaviour changes; update the relevant doc if you change
  a decision.
- By submitting a PR you agree to license your contribution under the project's
  dual [MIT](LICENSE-MIT) / [Apache-2.0](LICENSE-APACHE) license.

## Security

Please report security issues privately to atassikay38@gmail.com rather than via a
public issue.

## Code of conduct

Be respectful and constructive. Harassment or abuse isn't tolerated.

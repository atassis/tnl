# tnl Deployment Notes (v0.1.0-alpha)

These snippets describe how to wire Caddy in front of `tnld` for tunnel exposure.
v0.1.0-alpha does **not** ship a Dockerfile, compose file, CI, or smoke script —
those come in v0.1.0 (production).

## What this version requires

- A working wildcard certificate for `*.t.example.com` (DNS-01 with any provider
  supported by `caddy-dns/*` — choose your own). Spec §4 deliberately leaves
  this BYO.
- A wildcard A record `*.t.example.com` pointing to the host that runs Caddy.
- `tnld` running on the same host as Caddy, binding `127.0.0.1:7777` (default).
- `/etc/tnld/tokens.toml` populated with at least one argon2id-hashed token via
  `tnld hash-password <plaintext>`. Hand-edit the file in this version; the
  `tnld token add/list/revoke` admin CLI lands in v0.1.0-beta.

## Caddy snippets

Place these in `/opt/caddy/sites/`:

- [`tnl.caddy.example`](./tnl.caddy.example) → `tnl.caddy`
- [`tnl-api.caddy.example`](./tnl-api.caddy.example) → `tnl-api.caddy`

Replace `<provider>` and `{env.YOUR_DNS_TOKEN}` per your DNS provider's Caddy
plugin documentation. Reload Caddy with
`docker exec caddy caddy reload --config /etc/caddy/Caddyfile`.

## Manual end-to-end check

1. On the host, edit a `tokens.toml`:
   ```bash
   HASH=$(tnld hash-password tnl_DEMOSECRET)
   cat > /etc/tnld/tokens.toml <<EOF
   [[tokens]]
   name = "laptop"
   hash = "${HASH}"
   EOF
   chmod 600 /etc/tnld/tokens.toml
   ```
2. Write `/etc/tnld/config.toml`:
   ```toml
   listen        = "127.0.0.1:7777"
   public_url    = "https://tnl-api.t.example.com"
   hostname_root = "t.example.com"
   tokens_file   = "/etc/tnld/tokens.toml"
   ```
3. Start `tnld serve --config /etc/tnld/config.toml` (e.g. via tmux for now;
   systemd unit ships in v0.1.0).
4. On a client machine:
   ```bash
   tnl auth login --endpoint https://tnl-api.t.example.com --token tnl_DEMOSECRET
   python3 -m http.server 9999 &
   tnl http 9999 smoke
   ```
5. From a third terminal: `curl -sf https://smoke.t.example.com/`.

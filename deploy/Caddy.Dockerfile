# Caddy image with a caddy-dns provider plugin baked in.
#
# Why a custom image:
# - tnl needs a wildcard cert for *.<your-domain>.
# - Let's Encrypt only issues wildcards via the ACME DNS-01 challenge.
# - DNS-01 requires Caddy to write a TXT record into the zone, which needs the
#   matching DNS provider plugin compiled into the binary.
#
# To use YOUR provider: change the `--with` module below to the one for your DNS
# provider (browse https://github.com/caddy-dns), then build this image. The
# default is Cloudflare. The plugin reads its API token from TNL_DNS_TOKEN in the
# container environment (see deploy/Caddyfile and deploy/.env.example).
#
# Note on the floating `caddy:2-*` tags: xcaddy resolves transitive deps fresh on
# every build, and old caddy minors reference symbols removed in newer deps
# (e.g. zap >= ~1.27 dropped zapslog.HandlerOptions). The floating 2.x tag tracks
# an internally-consistent dependency set; hard-pinning a minor would eventually rot.

FROM caddy:2-builder AS builder
RUN xcaddy build \
	--with github.com/caddy-dns/cloudflare

FROM caddy:2-alpine
COPY --from=builder /usr/bin/caddy /usr/bin/caddy

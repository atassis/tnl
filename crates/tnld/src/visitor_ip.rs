//! Conservative daemon-authoritative visitor attribution. Trust is explicit;
//! the first untrusted hop scanning XFF right-to-left is the attribution boundary.

use std::net::IpAddr;

use anyhow::{bail, Context};
use axum::http::HeaderMap;
use ipnet::IpNet;
use tnl_protocol::request_metadata::{VisitorIdentity, VisitorIpSource};

const MAX_XFF_BYTES: usize = 4096;
const MAX_XFF_ADDRESSES: usize = 32;

#[derive(Debug)]
pub struct TrustedProxies {
    networks: Vec<IpNet>,
}

impl TrustedProxies {
    pub fn from_cidrs(cidrs: &[String]) -> anyhow::Result<Self> {
        let mut networks = Vec::with_capacity(cidrs.len());
        for entry in cidrs {
            if !entry.contains('/') {
                bail!("trusted_proxies entry {entry:?} must be a CIDR with a prefix");
            }
            let network: IpNet = entry
                .parse()
                .with_context(|| format!("invalid trusted_proxies entry {entry:?}"))?;
            if network.addr() != network.network() {
                bail!("trusted_proxies entry {entry:?} has host bits set");
            }
            if network.prefix_len() == 0 {
                tracing::warn!(cidr = %network, "trusted_proxies /0 loses the untrusted-peer boundary; strongly discouraged");
            }
            networks.push(network);
        }
        Ok(Self { networks })
    }

    fn contains(&self, address: IpAddr) -> bool {
        self.networks
            .iter()
            .any(|network| network.contains(&address))
    }

    pub fn resolve(&self, peer: IpAddr, headers: &HeaderMap) -> VisitorIdentity {
        let peer = peer.to_canonical();
        let fallback = |source| VisitorIdentity {
            visitor_ip: peer,
            peer_ip: peer,
            source,
        };
        if !self.contains(peer) {
            return fallback(VisitorIpSource::Peer);
        }
        let mut fields = headers.get_all("x-forwarded-for").iter();
        let Some(value) = fields.next() else {
            return fallback(VisitorIpSource::PeerMissingForwarding);
        };
        if fields.next().is_some() || value.as_bytes().len() > MAX_XFF_BYTES {
            return fallback(VisitorIpSource::PeerInvalidForwarding);
        }
        let Ok(text) = value.to_str() else {
            return fallback(VisitorIpSource::PeerInvalidForwarding);
        };
        let mut addresses = Vec::with_capacity(MAX_XFF_ADDRESSES);
        for token in text.split(',') {
            if addresses.len() == MAX_XFF_ADDRESSES {
                return fallback(VisitorIpSource::PeerInvalidForwarding);
            }
            let Ok(address) = token.trim_matches([' ', '\t']).parse::<IpAddr>() else {
                return fallback(VisitorIpSource::PeerInvalidForwarding);
            };
            addresses.push(address.to_canonical());
        }
        for address in addresses.into_iter().rev() {
            if !self.contains(address) {
                return VisitorIdentity {
                    visitor_ip: address,
                    peer_ip: peer,
                    source: VisitorIpSource::XForwardedFor,
                };
            }
        }
        fallback(VisitorIpSource::PeerAllTrusted)
    }
}

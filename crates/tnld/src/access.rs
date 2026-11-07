//! Bounded visitor-admission primitives. Not yet connected to registry/control
//! publication: protected creation remains unavailable until integration tests.

use std::{
    net::IpAddr,
    num::NonZeroU32,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};

use argon2::{password_hash::PasswordHash, Argon2, PasswordVerifier};
use axum::http::HeaderMap;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use governor::{clock::Clock as _, DefaultDirectRateLimiter, Quota, RateLimiter};
use http_body::{Body, Frame, SizeHint};
use ipnet::IpNet;
use tnl_protocol::access::{validate_username, PolicyError, VisitorAccessPolicyV1};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zeroize::Zeroizing;

const AUTH_WORKERS: usize = 4;
static GLOBAL_AUTH_WORKERS: OnceLock<AuthWorkers> = OnceLock::new();

/// Resolver fallbacks identify the proxy peer for diagnostics, not an available
/// end visitor. An enabled allowlist must not accidentally admit that proxy.
pub const fn visitor_ip_for_admission(
    visitor: &tnl_protocol::request_metadata::VisitorIdentity,
) -> Option<IpAddr> {
    use tnl_protocol::request_metadata::VisitorIpSource;
    match visitor.source {
        VisitorIpSource::Peer | VisitorIpSource::XForwardedFor => {
            Some(visitor.visitor_ip.to_canonical())
        }
        VisitorIpSource::PeerMissingForwarding
        | VisitorIpSource::PeerInvalidForwarding
        | VisitorIpSource::PeerAllTrusted => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccessDenial {
    #[error("visitor authentication required")]
    Basic,
    #[error("visitor network not allowed")]
    Ip,
    #[error("visitor rate limit exceeded")]
    Rate { retry_after: u64 },
    #[error("visitor concurrency limit exceeded")]
    Concurrency,
    #[error("visitor access unavailable")]
    Unavailable,
}

/// Shared ownership of a single exchange slot. The HTTP driver and response
/// body, or upgraded rendezvous/relay, each retain their own clone.
#[derive(Debug, Clone, Default)]
pub struct AdmissionLease {
    _permit: Option<Arc<OwnedSemaphorePermit>>,
}

#[derive(Debug)]
pub struct CompiledAccess {
    policy: Arc<VisitorAccessPolicyV1>,
    networks: Vec<IpNet>,
    rate: Option<DefaultDirectRateLimiter>,
    concurrency: Option<Arc<Semaphore>>,
}

impl CompiledAccess {
    pub fn new(policy: Arc<VisitorAccessPolicyV1>) -> Result<Self, PolicyError> {
        let networks = policy
            .allow_ips()
            .iter()
            .map(|s| s.parse().map_err(|_| PolicyError::Network))
            .collect::<Result<Vec<IpNet>, _>>()?;
        let rate = policy
            .rate_limit()
            .map(|limit| {
                let per_second =
                    NonZeroU32::new(limit.requests_per_second).ok_or(PolicyError::Limit)?;
                let burst = NonZeroU32::new(limit.burst).ok_or(PolicyError::Limit)?;
                Ok(RateLimiter::direct(
                    Quota::per_second(per_second).allow_burst(burst),
                ))
            })
            .transpose()?;
        let concurrency = policy
            .max_concurrent()
            .map(|limit| Arc::new(Semaphore::new(limit as usize)));
        Ok(Self {
            policy,
            networks,
            rate,
            concurrency,
        })
    }

    pub const fn policy(&self) -> &Arc<VisitorAccessPolicyV1> {
        &self.policy
    }

    /// Accept only a trusted resolver's canonical identity. The caller passes
    /// None when that resolver cannot establish an identity; never parse XFF here.
    pub async fn admit(
        &self,
        visitor_ip: Option<IpAddr>,
        headers: &mut HeaderMap,
    ) -> Result<AdmissionLease, AccessDenial> {
        if !self.networks.is_empty() {
            let ip = visitor_ip.ok_or(AccessDenial::Unavailable)?.to_canonical();
            if !self.networks.iter().any(|network| network.contains(&ip)) {
                return Err(AccessDenial::Ip);
            }
        }
        if let Some(rate) = &self.rate {
            if let Err(wait) = rate.check() {
                let duration = wait.wait_time_from(rate.clock().now());
                let retry_after = duration
                    .as_secs()
                    .saturating_add(u64::from(duration.subsec_nanos() != 0))
                    .max(1);
                return Err(AccessDenial::Rate { retry_after });
            }
        }
        if let Some(basic) = self.policy.basic() {
            let credentials = parse_basic(headers)?;
            let verifier = basic.password_hash().clone();
            let expected_username = basic.username().to_owned();
            let authenticated = GLOBAL_AUTH_WORKERS
                .get_or_init(|| AuthWorkers::new(AUTH_WORKERS))
                .run(move || {
                    // Always perform the bounded hash for well-formed credentials,
                    // even when the username is wrong. PHC was validated at compile.
                    let password_ok = PasswordHash::new(verifier.expose_for_verification())
                        .is_ok_and(|hash| {
                            Argon2::default()
                                .verify_password(&credentials.password, &hash)
                                .is_ok()
                        });
                    password_ok && (credentials.username == expected_username)
                })
                .await?;
            if !authenticated {
                return Err(AccessDenial::Basic);
            }
            // Remove ALL values only for a successfully enabled Basic gate.
            headers.remove(axum::http::header::AUTHORIZATION);
        }
        let permit = self
            .concurrency
            .as_ref()
            .map(|semaphore| {
                semaphore
                    .clone()
                    .try_acquire_owned()
                    .map(Arc::new)
                    .map_err(|_| AccessDenial::Concurrency)
            })
            .transpose()?;
        Ok(AdmissionLease { _permit: permit })
    }
}

#[derive(Debug, Clone)]
struct AuthWorkers {
    permits: Arc<Semaphore>,
}

impl AuthWorkers {
    fn new(capacity: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(capacity)),
        }
    }

    async fn run<T, F>(&self, work: F) -> Result<T, AccessDenial>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| AccessDenial::Unavailable)?;
        tokio::task::spawn_blocking(move || {
            // Cancellation of the async waiter must not free a slot while the
            // blocking hash still owns its memory/CPU. Never queue without one.
            let _permit = permit;
            work()
        })
        .await
        .map_err(|_| AccessDenial::Unavailable)
    }
}

struct BasicCredentials {
    username: String,
    password: Zeroizing<Vec<u8>>,
}

fn parse_basic(headers: &HeaderMap) -> Result<BasicCredentials, AccessDenial> {
    let mut fields = headers.get_all(axum::http::header::AUTHORIZATION).iter();
    let field = fields.next().ok_or(AccessDenial::Basic)?;
    if fields.next().is_some() || field.as_bytes().len() > 2048 {
        return Err(AccessDenial::Basic);
    }
    let field = field.to_str().map_err(|_| AccessDenial::Basic)?;
    let (scheme, token) = field.split_once(' ').ok_or(AccessDenial::Basic)?;
    if !scheme.eq_ignore_ascii_case("basic")
        || token.is_empty()
        || token.bytes().any(|b| b.is_ascii_whitespace())
    {
        return Err(AccessDenial::Basic);
    }
    let decoded = Zeroizing::new(STANDARD.decode(token).map_err(|_| AccessDenial::Basic)?);
    let decoded = std::str::from_utf8(&decoded).map_err(|_| AccessDenial::Basic)?;
    let (username, password) = decoded.split_once(':').ok_or(AccessDenial::Basic)?;
    validate_username(username).map_err(|_| AccessDenial::Basic)?;
    if password.is_empty() || password.len() > 1024 || password.chars().any(char::is_control) {
        return Err(AccessDenial::Basic);
    }
    Ok(BasicCredentials {
        username: username.to_owned(),
        password: Zeroizing::new(password.as_bytes().to_owned()),
    })
}

/// Trailer-preserving body owner; a separate driver/relay lease must also be
/// retained so early response EOF cannot release an ongoing upload or upgrade.
#[derive(Debug)]
pub struct LeasedBody<B> {
    inner: Pin<Box<B>>,
    lease: Option<AdmissionLease>,
}

impl<B> LeasedBody<B> {
    pub fn new(body: B, lease: AdmissionLease) -> Self {
        Self {
            inner: Box::pin(body),
            lease: Some(lease),
        }
    }
}

impl<B: Body> Body for LeasedBody<B> {
    type Data = B::Data;
    type Error = B::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let result = self.as_mut().get_mut().inner.as_mut().poll_frame(cx);
        if matches!(&result, Poll::Ready(None | Some(Err(_)))) {
            self.get_mut().lease.take();
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};
    use tnl_protocol::access::{BasicVerifier, SecretVerifier, VisitorRateLimit};

    fn policy(
        basic: bool,
        rate: bool,
        concurrent: bool,
        networks: &[&str],
    ) -> Arc<VisitorAccessPolicyV1> {
        let basic = basic.then(|| {
            BasicVerifier::new(
                "user".into(),
                SecretVerifier::new(crate::auth::hash_plaintext(" secret:pass ").unwrap()).unwrap(),
            )
            .unwrap()
        });
        Arc::new(
            VisitorAccessPolicyV1::new(
                basic,
                &networks.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
                rate.then_some(VisitorRateLimit {
                    requests_per_second: 1,
                    burst: 2,
                }),
                concurrent.then_some(1),
            )
            .unwrap(),
        )
    }

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_str(value).unwrap());
        headers
    }

    fn basic_header(user: &str, password: &str) -> HeaderMap {
        use base64::Engine as _;
        headers(&format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        ))
    }

    #[tokio::test]
    async fn basic_only_consumes_gate_header_and_checks_password_even_without_limits() {
        let access = CompiledAccess::new(policy(true, false, false, &[])).unwrap();
        let mut req_headers = basic_header("user", " secret:pass ");
        let lease = access.admit(None, &mut req_headers).await.unwrap();
        assert!(!req_headers.contains_key("authorization"));
        drop(lease);
        for mut bad in [
            basic_header("other", " secret:pass "),
            basic_header("user", "bad"),
            headers("Bearer SECRET_SENTINEL"),
        ] {
            assert!(matches!(
                access.admit(None, &mut bad).await,
                Err(AccessDenial::Basic)
            ));
        }
    }

    #[tokio::test]
    async fn allow_only_uses_typed_identity_not_spoofed_headers_and_preserves_authorization() {
        let access = CompiledAccess::new(policy(false, false, false, &["192.0.2.0/24"])).unwrap();
        let mut request_headers = headers("Bearer APP_SENTINEL");
        request_headers.insert("x-forwarded-for", HeaderValue::from_static("192.0.2.1"));
        assert!(matches!(
            access
                .admit(Some("198.51.100.1".parse().unwrap()), &mut request_headers)
                .await,
            Err(AccessDenial::Ip)
        ));
        assert!(matches!(
            access.admit(None, &mut request_headers).await,
            Err(AccessDenial::Unavailable)
        ));
        access
            .admit(
                Some("::ffff:192.0.2.9".parse().unwrap()),
                &mut request_headers,
            )
            .await
            .unwrap();
        assert_eq!(request_headers["authorization"], "Bearer APP_SENTINEL");
    }

    #[tokio::test]
    async fn aggregate_burst_is_shared_but_allowlist_denial_does_not_spend_it() {
        let access = CompiledAccess::new(policy(false, true, false, &["192.0.2.0/24"])).unwrap();
        let mut h = HeaderMap::new();
        assert!(access
            .admit(Some("198.51.100.1".parse().unwrap()), &mut h)
            .await
            .is_err());
        for _ in 0..2 {
            access
                .admit(Some("192.0.2.1".parse().unwrap()), &mut h)
                .await
                .unwrap();
        }
        assert!(matches!(
            access
                .admit(Some("192.0.2.2".parse().unwrap()), &mut h)
                .await,
            Err(AccessDenial::Rate { retry_after: 1 })
        ));
    }

    #[tokio::test]
    async fn concurrency_lease_is_released_only_after_final_owner() {
        let access = CompiledAccess::new(policy(false, false, true, &[])).unwrap();
        let mut h = headers("Bearer APP_SENTINEL");
        let lease = access.admit(None, &mut h).await.unwrap();
        let relay_owner = lease.clone();
        drop(lease);
        assert!(matches!(
            access.admit(None, &mut h).await,
            Err(AccessDenial::Concurrency)
        ));
        drop(relay_owner);
        access.admit(None, &mut h).await.unwrap();
        assert_eq!(h["authorization"], "Bearer APP_SENTINEL");
    }

    #[tokio::test]
    async fn blocking_worker_capacity_survives_request_cancellation_without_queueing() {
        let workers = AuthWorkers::new(1);
        let work = workers.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (ended_tx, ended_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            work.run(move || {
                let _ = started_tx.send(());
                release_rx.recv().unwrap();
                let _ = ended_tx.send(());
                true
            })
            .await
        });
        started_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        let denied = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            workers.run(|| panic!("must not queue")),
        )
        .await
        .unwrap();
        assert!(matches!(denied, Err(AccessDenial::Unavailable)));
        release_tx.send(()).unwrap();
        ended_rx.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if workers.permits.available_permits() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(workers.run(|| true).await.unwrap());
    }

    #[test]
    fn malformed_basic_is_bounded_generic_and_duplicate_headers_are_rejected() {
        let mut duplicate = basic_header("user", "pass");
        duplicate.append(
            "authorization",
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        for input in [
            HeaderMap::new(),
            duplicate,
            headers("Basic !!!"),
            headers("Basic dXNlcjpwYXNz extra"),
            headers("Basic dXNlcjpwYXN"),
            headers(&format!("Basic {}", "x".repeat(2048))),
        ] {
            let result = parse_basic(&input);
            assert!(result.is_err());
        }
        let credentials = parse_basic(&basic_header("user", "a:b")).unwrap();
        assert_eq!(&*credentials.password, b"a:b");
    }
    #[tokio::test]
    async fn trusted_proxy_fallback_is_unavailable_for_allowlist_not_a_proxy_ip_bypass() {
        let resolver =
            crate::visitor_ip::TrustedProxies::from_cidrs(&["127.0.0.0/8".into()]).unwrap();
        let access = CompiledAccess::new(policy(false, false, false, &["127.0.0.0/8"])).unwrap();
        for mut h in [HeaderMap::new(), headers("Bearer APP_SENTINEL")] {
            h.insert("x-forwarded-for", HeaderValue::from_static("not an IP"));
            let visitor = resolver.resolve("127.0.0.1".parse().unwrap(), &h);
            assert_eq!(visitor_ip_for_admission(&visitor), None);
            assert!(matches!(
                access
                    .admit(visitor_ip_for_admission(&visitor), &mut h)
                    .await,
                Err(AccessDenial::Unavailable)
            ));
        }
        let h = HeaderMap::new();
        let missing = resolver.resolve("127.0.0.1".parse().unwrap(), &h);
        assert_eq!(visitor_ip_for_admission(&missing), None);
        let visitor = resolver.resolve(
            "198.51.100.1".parse().unwrap(),
            &headers("Bearer APP_SENTINEL"),
        );
        assert_eq!(
            visitor_ip_for_admission(&visitor),
            Some("198.51.100.1".parse().unwrap())
        );
    }

    #[derive(Debug)]
    struct Frames(std::collections::VecDeque<Result<Frame<bytes::Bytes>, std::io::Error>>);
    impl Body for Frames {
        type Data = bytes::Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(self.0.pop_front())
        }
    }

    #[tokio::test]
    async fn response_body_preserves_trailers_and_driver_owner_outlives_body_eof() {
        let access = CompiledAccess::new(policy(false, false, true, &[])).unwrap();
        let mut h = HeaderMap::new();
        let driver_owner = access.admit(None, &mut h).await.unwrap();
        let mut trailers = HeaderMap::new();
        trailers.insert("x-checksum", HeaderValue::from_static("complete"));
        let frames = Frames(
            [
                Ok(Frame::data(bytes::Bytes::from_static(b"body"))),
                Ok(Frame::trailers(trailers)),
            ]
            .into(),
        );
        let mut body = LeasedBody::new(frames, driver_owner.clone());
        let data = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(data.data_ref().unwrap(), "body");
        let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.trailers_ref().unwrap()["x-checksum"], "complete");
        assert!(
            std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
                .await
                .is_none()
        );
        drop(body);
        assert!(matches!(
            access.admit(None, &mut h).await,
            Err(AccessDenial::Concurrency)
        ));
        drop(driver_owner);
        access.admit(None, &mut h).await.unwrap();
    }

    #[tokio::test]
    async fn response_body_error_and_drop_release_their_lease_without_an_orphan() {
        let access = CompiledAccess::new(policy(false, false, true, &[])).unwrap();
        let mut h = HeaderMap::new();
        let mut body = LeasedBody::new(
            Frames([Err(std::io::ErrorKind::ConnectionReset.into())].into()),
            access.admit(None, &mut h).await.unwrap(),
        );
        assert!(
            std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
                .await
                .unwrap()
                .is_err()
        );
        let body = LeasedBody::new(
            Frames(std::collections::VecDeque::new()),
            access.admit(None, &mut h).await.unwrap(),
        );
        drop(body);
        access.admit(None, &mut h).await.unwrap();
    }
}

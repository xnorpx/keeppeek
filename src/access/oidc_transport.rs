//! Restricts OIDC network requests to configured HTTPS origins and approved addresses.

use super::external::Oidc;
use anyhow::{Result, ensure};
use ipnet::IpNet;
use oauth2::{HttpRequest, HttpResponse};
use std::{
    collections::HashSet,
    fmt,
    net::IpAddr,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use ureq::unversioned::{
    resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver},
    transport::{DefaultConnector, NextTimeout},
};
use url::Url;

const RESPONSE_LIMIT: u64 = 65_536;

#[derive(Clone, Debug)]
pub struct Policy {
    origins: HashSet<String>,
    private_networks: Vec<IpNet>,
}

impl Policy {
    pub(crate) fn new(config: &Oidc) -> Result<Self> {
        let issuer = Url::parse(&config.issuer)?;
        let mut origins: HashSet<_> = config.endpoint_origins.iter().cloned().collect();
        origins.insert(issuer.origin().ascii_serialization());
        Ok(Self {
            origins,
            private_networks: config.private_networks.clone(),
        })
    }

    pub(crate) fn endpoint(&self, value: &str) -> Result<()> {
        ensure!(value.len() <= 2_048, "OIDC endpoint exceeds the limit");
        let url = Url::parse(value).map_err(|_| anyhow::anyhow!("invalid OIDC endpoint"))?;
        ensure!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
                && self.origins.contains(&url.origin().ascii_serialization()),
            "OIDC endpoint is not allowed"
        );
        Ok(())
    }

    fn address(&self, address: IpAddr) -> bool {
        let address = super::normalize_address(address);
        let private = match address {
            IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
            IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
        };
        if private {
            return self
                .private_networks
                .iter()
                .any(|network| network.contains(&address));
        }
        !denied_networks()
            .iter()
            .any(|network| network.contains(&address))
            && match address {
                IpAddr::V4(_) => true,
                IpAddr::V6(ip) => ip.segments()[0] & 0xe000 == 0x2000,
            }
    }
}

fn denied_networks() -> &'static [IpNet] {
    static NETWORKS: OnceLock<Vec<IpNet>> = OnceLock::new();
    NETWORKS.get_or_init(|| {
        // Special-use destinations must not become metadata-service or multicast targets.
        [
            "0.0.0.0/8",
            "100.64.0.0/10",
            "169.254.0.0/16",
            "192.0.0.0/24",
            "192.0.2.0/24",
            "192.88.99.0/24",
            "198.18.0.0/15",
            "198.51.100.0/24",
            "203.0.113.0/24",
            "224.0.0.0/3",
            "2001::/23",
            "2001:db8::/32",
            "2002::/16",
            "3fff::/20",
        ]
        .into_iter()
        .map(|network| network.parse().expect("fixed network must parse"))
        .collect()
    })
}

impl Resolver for Policy {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> std::result::Result<ResolvedSocketAddrs, ureq::Error> {
        if self.endpoint(&uri.to_string()).is_err() {
            return Err(ureq::Error::HostNotFound);
        }
        let addresses = DefaultResolver::default().resolve(uri, config, timeout)?;
        if addresses.iter().any(|address| !self.address(address.ip())) {
            return Err(ureq::Error::HostNotFound);
        }
        // Pass the checked addresses directly to the connector; never resolve them a second time.
        Ok(addresses)
    }
}

#[derive(Clone)]
pub struct Transport {
    agent: ureq::Agent,
    policy: Policy,
    in_flight: Arc<Mutex<()>>,
}

#[derive(Debug)]
pub struct TransportError;

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OIDC provider request failed")
    }
}

impl std::error::Error for TransportError {}

impl Transport {
    pub(crate) fn new(config: &Oidc) -> Result<Self> {
        Self::with_roots(config, ureq::tls::RootCerts::PlatformVerifier)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        config: &Oidc,
        certificate: ureq::tls::Certificate<'static>,
    ) -> Result<Self> {
        Self::with_roots(config, ureq::tls::RootCerts::new_with_certs(&[certificate]))
    }

    fn with_roots(config: &Oidc, roots: ureq::tls::RootCerts) -> Result<Self> {
        let policy = Policy::new(config)?;
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .max_response_header_size(32_768)
            .timeout_global(Some(Duration::from_secs(30)))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .root_certs(roots)
                    .build(),
            )
            .build();
        let agent = ureq::Agent::with_parts(config, DefaultConnector::default(), policy.clone());
        Ok(Self {
            agent,
            policy,
            in_flight: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) fn request(
        &self,
        request: HttpRequest,
    ) -> std::result::Result<HttpResponse, TransportError> {
        let _permit = self.in_flight.try_lock().map_err(|_| TransportError)?;
        if request.body().len() > RESPONSE_LIMIT as usize
            || !matches!(request.method().as_str(), "GET" | "POST")
            || self.policy.endpoint(&request.uri().to_string()).is_err()
        {
            return Err(TransportError);
        }
        let response = self.agent.run(request).map_err(|_| TransportError)?;
        if response.status().is_redirection() || response.status().is_server_error() {
            return Err(TransportError);
        }
        let (parts, body) = response.into_parts();
        let bytes = body
            .into_with_config()
            .limit(RESPONSE_LIMIT)
            .read_to_vec()
            .map_err(|_| TransportError)?;
        Ok(HttpResponse::from_parts(parts, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::Policy;

    #[test]
    fn https_transport_verifies_certificates_and_bounds_redirects_and_bodies() {
        use crate::access::oidc_fixture::{Fixture, Response};
        let fixture = Fixture::new(|request| match request.path.as_str() {
            "/huge" => Response {
                status: 200,
                headers: vec![],
                body: vec![b'x'; 65_537],
            },
            "/redirect" => Response {
                status: 302,
                headers: vec![("Location".into(), "https://evil.example/".into())],
                body: vec![],
            },
            _ => Response::json(&serde_json::json!({"verified": true})),
        });
        let config: super::Oidc = toml::from_str(&format!("issuer = '{}'\nclient_id = 'keeppeek'\nredirect_uri = 'https://keeppeek.example/auth/callback'\nprivate_networks = ['127.0.0.1/32']", fixture.origin)).unwrap();
        let transport = super::Transport::for_test(&config, fixture.certificate.clone()).unwrap();
        let request = |path| {
            oauth2::http::Request::builder()
                .uri(format!("{}{path}", fixture.origin))
                .body(vec![])
                .unwrap()
        };
        assert_eq!(
            transport
                .agent
                .run(request("/ok"))
                .expect("TLS fixture request failed")
                .status(),
            200
        );
        assert_eq!(transport.request(request("/ok")).unwrap().status(), 200);
        assert!(transport.request(request("/huge")).is_err());
        assert!(transport.request(request("/redirect")).is_err());
        assert!(
            super::Transport::new(&config)
                .unwrap()
                .request(request("/ok"))
                .is_err()
        );
        let _busy = transport.in_flight.lock().unwrap();
        assert!(transport.request(request("/ok")).is_err());
    }

    fn policy() -> Policy {
        Policy::new(
            &toml::from_str::<super::Oidc>(
                r#"
issuer = "https://identity.example/tenant"
client_id = "keeppeek"
redirect_uri = "https://keeppeek.example/auth/callback"
endpoint_origins = ["https://keys.example"]
private_networks = ["10.20.0.0/16", "127.0.0.1/32", "::1/128"]
"#,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn destination_policy_pins_https_origins_and_rejects_url_credentials() {
        let policy = policy();
        for url in [
            "https://identity.example/token",
            "https://keys.example/jwks",
        ] {
            assert!(policy.endpoint(url).is_ok());
        }
        for url in [
            "http://identity.example/token",
            "https://evil.example",
            "https://identity.example:444/token",
            "https://user:pass@identity.example",
            "https://identity.example/#fragment",
        ] {
            assert!(policy.endpoint(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn resolved_addresses_must_be_public_or_explicit_private_exceptions() {
        let policy = policy();
        for ip in [
            "8.8.8.8",
            "2606:4700::1111",
            "10.20.0.8",
            "127.0.0.1",
            "::1",
        ] {
            assert!(policy.address(ip.parse().unwrap()), "rejected {ip}");
        }
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "172.16.1.1",
            "192.168.1.1",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "fe80::1",
            "fc00::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:169.254.169.254",
        ] {
            assert!(!policy.address(ip.parse().unwrap()), "accepted {ip}");
        }
    }
}

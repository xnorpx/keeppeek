use super::FakeHikvision;
use digest_auth::{AuthContext, HttpMethod};
use std::time::Duration;

/// A bounded callback result without response-body or credential-bearing diagnostics.
#[derive(Debug)]
pub struct CallbackResponse {
    status: u16,
}

impl CallbackResponse {
    /// Returns the receiver's final HTTP status after the optional Digest challenge.
    pub const fn status(&self) -> u16 {
        self.status
    }
}

impl FakeHikvision {
    /// Opens a loopback receiver socket for partial-upload and malformed-request scenarios.
    pub fn callback_connection(
        &self,
        address: std::net::SocketAddr,
    ) -> anyhow::Result<std::net::TcpStream> {
        anyhow::ensure!(
            address.ip().is_loopback(),
            "fake callbacks may only contact loopback"
        );
        let socket = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
        Ok(socket)
    }
    /// Sends a synthetic callback to a loopback-only receiver using Digest authentication.
    ///
    /// Each call is one delivery attempt. HTTP failures are returned, not retried as successful events.
    pub fn post_callback(
        &self,
        destination: &str,
        media_type: &str,
        body: &[u8],
        username: &str,
        password: &str,
    ) -> anyhow::Result<CallbackResponse> {
        let url = url::Url::parse(destination)?;
        anyhow::ensure!(
            url.scheme() == "http"
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "fake callback requires a credential-free HTTP URL"
        );
        anyhow::ensure!(
            matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
                || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback()),
            "fake callbacks may only contact loopback"
        );
        anyhow::ensure!(
            body.len() <= 8 * 1024 * 1024,
            "fake callback exceeds body limit"
        );
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .new_agent();
        let response = agent
            .post(destination)
            .header("content-type", media_type)
            .send(body)?;
        if response.status().as_u16() != 401 {
            return Ok(CallbackResponse {
                status: response.status().as_u16(),
            });
        }
        let challenge = response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                anyhow::anyhow!("fake callback receiver did not issue a Digest challenge")
            })?;
        let mut challenge = digest_auth::parse(challenge)?;
        let target = &url[url::Position::BeforePath..];
        let mut context =
            AuthContext::new_with_method(username, password, target, Some(body), HttpMethod::POST);
        context.set_custom_cnonce("fake-hikvision-callback");
        let authorization = challenge.respond(&context)?.to_string();
        drop(response);
        let response = agent
            .post(destination)
            .header("content-type", media_type)
            .header("authorization", authorization)
            .send(body)?;
        Ok(CallbackResponse {
            status: response.status().as_u16(),
        })
    }
}

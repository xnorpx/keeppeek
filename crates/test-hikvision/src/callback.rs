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
            .timeout_await_100(Some(Duration::from_secs(1)))
            .build()
            .new_agent();
        let response = agent
            .post(destination)
            .header("content-type", media_type)
            .header("expect", "100-continue")
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
            .header("expect", "100-continue")
            .send(body)?;
        Ok(CallbackResponse {
            status: response.status().as_u16(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn callback_rejection_is_received_before_any_body_upload() {
        let receiver = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = receiver.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = receiver.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut headers = String::new();
            for _ in 0..33 {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                headers.push_str(&line);
                assert!(headers.len() <= 16 * 1024);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("expect: 100-continue\r\n")
            );
            assert!(
                reader.buffer().is_empty(),
                "body must wait for receiver admission"
            );
            socket
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let camera = FakeHikvision::builder().start().unwrap();
        let response = camera.post_callback(
            &format!("http://{address}/events"),
            "application/json",
            &vec![b' '; 512 * 1024],
            "test",
            "test",
        );
        server.join().unwrap();
        assert_eq!(response.unwrap().status(), 400);
    }
}

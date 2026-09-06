use std::time::{Duration, Instant};

use digest_auth::{AuthContext, HttpMethod};
use ureq::http::Response;

use super::Endpoint;
use super::client::{
    AUTH_ATTEMPTS_MAX, Client, ClientError, Failure, validate_body_headers, validate_unique_headers,
};

const JPEG_SIZE_BYTES_MAX: u64 = 1024 * 1024;
const TIMEOUT_MAX: Duration = Duration::from_secs(5);

impl Client {
    /// Fetches a bounded JPEG from an approved camera endpoint using HTTP GET.
    ///
    /// The positive `timeout` must not exceed five seconds. One deadline covers
    /// authentication and body reads, with at most three attempts. This method
    /// reuses the current origin's Digest challenge and nonce but never sends a SOAP
    /// UsernameToken or Basic credentials. Redirects and proxies remain disabled.
    /// JPEG data is limited to 1 MiB and checked for SOI and EOI markers. Callers
    /// must decode the image separately to validate its dimensions and contents.
    ///
    /// # Errors
    /// Rejects invalid endpoints, timeouts, authentication, HTTP status, headers,
    /// oversized bodies and malformed JPEG boundaries without exposing payloads.
    pub fn snapshot(
        &mut self,
        endpoint: &Endpoint,
        timeout: Duration,
    ) -> Result<Vec<u8>, ClientError> {
        if timeout.is_zero() || timeout > TIMEOUT_MAX {
            return Err(ClientError(Failure::Protocol));
        }
        let deadline = Instant::now() + timeout;
        self.camera
            .resolve(endpoint.as_str())
            .map_err(|_| ClientError(Failure::Protocol))?;
        let url = self.prepare_auth(endpoint)?;
        let target = &url[url::Position::BeforePath..url::Position::AfterQuery];
        for attempt in 0..AUTH_ATTEMPTS_MAX {
            let response = self.snapshot_get(endpoint, target, deadline)?;
            validate_unique_headers(response.headers())?;
            remaining(deadline)?;
            if response.status().as_u16() == 401 {
                self.accept_challenge(response.headers(), attempt)?;
                continue;
            }
            return read_jpeg(response, deadline);
        }
        Err(ClientError(Failure::Authentication))
    }

    fn snapshot_get(
        &mut self,
        endpoint: &Endpoint,
        target: &str,
        deadline: Instant,
    ) -> Result<Response<ureq::Body>, ClientError> {
        let mut request = self
            .agent
            .get(endpoint.as_str())
            .header("Accept", "image/jpeg")
            .header("Accept-Encoding", "identity");
        if let Some(challenge) = &mut self.challenge {
            let mut context = AuthContext::new_with_method(
                self.credentials.username.as_str(),
                self.credentials.password.as_str(),
                target,
                Some(&[]),
                HttpMethod::GET,
            );
            context.set_custom_cnonce(self.nonce.clone());
            let authorization = challenge
                .respond(&context)
                .map_err(|_| ClientError(Failure::Authentication))?;
            request = request.header("Authorization", authorization.to_string());
        }
        request
            .config()
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_global(Some(remaining(deadline)?))
            .build()
            .call()
            .map_err(|_| ClientError(Failure::Network))
    }
}

fn remaining(deadline: Instant) -> Result<Duration, ClientError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(ClientError(Failure::Network))
}

fn read_jpeg(response: Response<ureq::Body>, deadline: Instant) -> Result<Vec<u8>, ClientError> {
    let status = response.status().as_u16();
    if status != 200 {
        return Err(ClientError(Failure::Http(status)));
    }
    validate_body_headers(response.headers(), "image/jpeg", JPEG_SIZE_BYTES_MAX)?;
    let bytes = response
        .into_body()
        .into_with_config()
        .limit(JPEG_SIZE_BYTES_MAX + 1)
        .read_to_vec()
        .map_err(|error| {
            ClientError(match error {
                ureq::Error::BodyExceedsLimit(_) => Failure::Protocol,
                _ => Failure::Network,
            })
        })?;
    remaining(deadline)?;
    if bytes.len() as u64 > JPEG_SIZE_BYTES_MAX
        || !bytes.starts_with(&[0xff, 0xd8])
        || !bytes.ends_with(&[0xff, 0xd9])
    {
        return Err(ClientError(Failure::Protocol));
    }
    Ok(bytes)
}

use std::io::Read;
use std::time::Instant;

use super::{Client, REQUEST_TIMEOUT};
use crate::error::Kind;
use crate::management::{Query, ResponseStatus};
use crate::{Error, Part, PartKind, Request, XML_SIZE_BYTES_MAX};

impl Client {
    /// Executes a typed query and validates the expected resource and device status.
    ///
    /// # Errors
    /// Returns transport, HTTP, device-level or response-schema errors.
    pub fn query<Output>(&mut self, query: &Query<Output>) -> Result<Output, Error> {
        self.query_until(query, Instant::now() + REQUEST_TIMEOUT)
    }

    pub(super) fn query_until<Output>(
        &mut self,
        query: &Query<Output>,
        deadline: Instant,
    ) -> Result<Output, Error> {
        let part = self.exchange_until(query.request(), XML_SIZE_BYTES_MAX, deadline)?;
        query.parse(&content_type(&part), part.body())
    }

    /// Executes an explicit command and requires a successful device ResponseStatus.
    ///
    /// A transport error leaves the device outcome unknown. This method does not
    /// automatically retry a failed write or reboot a camera.
    ///
    /// # Errors
    /// Rejects HTTP or device failures, missing status bodies and malformed responses.
    pub fn command(&mut self, request: &Request) -> Result<ResponseStatus, Error> {
        self.command_until(request, Instant::now() + REQUEST_TIMEOUT)
    }

    pub(super) fn command_until(
        &mut self,
        request: &Request,
        deadline: Instant,
    ) -> Result<ResponseStatus, Error> {
        let part = self.exchange_until(request, XML_SIZE_BYTES_MAX, deadline)?;
        let status = ResponseStatus::parse(&content_type(&part), part.body())?;
        status.ensure_success()?;
        Ok(status)
    }

    /// Fetches an original JPEG snapshot for a streaming channel with a one-MiB limit.
    ///
    /// # Errors
    /// Rejects stream zero, device failures, wrong media types and invalid JPEG framing.
    pub fn snapshot(&mut self, stream: u32) -> Result<Vec<u8>, Error> {
        if stream == 0 {
            return Err(Error::new(Kind::InvalidInput));
        }
        let part = self.exchange(
            &Request::get(format!("/ISAPI/Streaming/channels/{stream}/picture"))?,
            1024 * 1024,
        )?;
        if part.kind() != PartKind::Jpeg
            || !part.body().starts_with(&[0xff, 0xd8])
            || !part.body().ends_with(&[0xff, 0xd9])
            || part.body().len() < 4
        {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(part.into_body())
    }

    fn exchange(&mut self, request: &Request, limit: usize) -> Result<Part, Error> {
        self.exchange_until(request, limit, Instant::now() + REQUEST_TIMEOUT)
    }

    fn exchange_until(
        &mut self,
        request: &Request,
        limit: usize,
        deadline: Instant,
    ) -> Result<Part, Error> {
        let response = self.open(request, Some(deadline), &self.agent.clone())?;
        let status = response.status().as_u16();
        let mut types = response.headers().get_all("content-type").iter();
        let media = types
            .next()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        if types.next().is_some() {
            return Err(Error::new(Kind::Protocol));
        }
        let mut body = Vec::new();
        response
            .into_body()
            .into_with_config()
            .reader()
            .take(limit as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > limit {
            return Err(Error::new(Kind::Limit));
        }
        if let Ok(device) = ResponseStatus::parse(&media, &body) {
            device.ensure_success()?;
        }
        if !(200..300).contains(&status) {
            return Err(Error::new(Kind::HttpStatus(status)));
        }
        Part::from_body(&media, body)
    }
}

fn content_type(part: &Part) -> String {
    part.charset().map_or_else(
        || part.media_type().to_owned(),
        |charset| format!("{}; charset={charset}", part.media_type()),
    )
}

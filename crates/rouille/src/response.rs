// Copyright (c) 2016 The Rouille developers
// Licensed under the Apache License, Version 2.0
// <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT
// license <LICENSE-MIT or http://opensource.org/licenses/MIT>,
// at your option. All files in the project carrying such
// notice may not be copied, modified, or distributed except
// according to those terms.

use crate::Request;
use crate::Upgrade;
use std::borrow::Cow;
use std::fmt;
use std::fs::File;
use std::io;
use std::io::Cursor;
use std::io::Read;
use std::io::{Seek, SeekFrom};

/// Contains a prototype of a response.
///
/// The response is only sent to the client when you return the `Response` object from your
/// request handler. This means that you are free to create as many `Response` objects as you want.
pub struct Response {
    /// The status code to return to the user.
    pub status_code: u16,

    /// List of headers to be returned in the response.
    ///
    /// The value of the following headers will be ignored from this list, even if present:
    ///
    /// - Connection
    /// - Content-Length
    /// - Trailer
    /// - Transfer-Encoding
    ///
    /// Additionally, the `Upgrade` header is ignored as well unless the `upgrade` field of the
    /// `Response` is set to something.
    ///
    /// The reason for this is that these headers are too low-level and are directly handled by
    /// the underlying HTTP response system.
    ///
    /// The value of `Content-Length` is automatically determined by the `ResponseBody` object of
    /// the `data` member.
    ///
    /// If you want to send back `Connection: upgrade`, you should set the value of the `upgrade`
    /// field to something.
    pub headers: Vec<(Cow<'static, str>, Cow<'static, str>)>,

    /// An opaque type that contains the body of the response.
    pub data: ResponseBody,

    /// If set, rouille will give ownership of the client socket to the `Upgrade` object.
    ///
    /// In all circumstances, the value of the `Connection` header is managed by the framework and
    /// cannot be customized. If this value is set, the response will automatically contain
    /// `Connection: Upgrade`.
    pub upgrade: Option<Box<dyn Upgrade + Send>>,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Response")
            .field("status_code", &self.status_code)
            .field("headers", &self.headers)
            .finish()
    }
}

impl Response {
    /// Returns true if the status code of this `Response` indicates success.
    ///
    /// This is the range [200-399].
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::text("hello world");
    /// assert!(response.is_success());
    /// ```
    #[inline]
    pub const fn is_success(&self) -> bool {
        self.status_code >= 200 && self.status_code < 400
    }

    /// Shortcut for `!response.is_success()`.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::empty_400();
    /// assert!(response.is_error());
    /// ```
    #[inline]
    pub const fn is_error(&self) -> bool {
        !self.is_success()
    }

    /// Builds a `Response` that redirects the user to another URL with a 301 status code. This
    /// semantically means a permanent redirect.
    ///
    /// > **Note**: If you're uncertain about which status code to use for a redirection, 303 is
    /// > the safest choice.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::redirect_301("/foo");
    /// ```
    #[inline]
    pub fn redirect_301<S>(target: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        Self {
            status_code: 301,
            headers: vec![("Location".into(), target.into())],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds a `Response` that redirects the user to another URL with a 302 status code. This
    /// semantically means a temporary redirect.
    ///
    /// > **Note**: If you're uncertain about which status code to use for a redirection, 303 is
    /// > the safest choice.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::redirect_302("/bar");
    /// ```
    #[inline]
    pub fn redirect_302<S>(target: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        Self {
            status_code: 302,
            headers: vec![("Location".into(), target.into())],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds a `Response` that redirects the user to another URL with a 303 status code. This
    /// means "See Other" and is usually used to indicate where the response of a query is
    /// located.
    ///
    /// For example when a user sends a POST request to URL `/foo` the server can return a 303
    /// response with a target to `/bar`, in which case the browser will automatically change
    /// the page to `/bar` (with a GET request to `/bar`).
    ///
    /// > **Note**: If you're uncertain about which status code to use for a redirection, 303 is
    /// > the safest choice.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let user_id = 5;
    /// let response = Response::redirect_303(format!("/users/{}", user_id));
    /// ```
    #[inline]
    pub fn redirect_303<S>(target: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        Self {
            status_code: 303,
            headers: vec![("Location".into(), target.into())],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds a `Response` that redirects the user to another URL with a 307 status code. This
    /// semantically means a permanent redirect.
    ///
    /// The difference between 307 and 301 is that the client must keep the same method after
    /// the redirection. For example if the browser sends a POST request to `/foo` and that route
    /// returns a 307 redirection to `/bar`, then the browser will make a POST request to `/bar`.
    /// With a 301 redirection it would use a GET request instead.
    ///
    /// > **Note**: If you're uncertain about which status code to use for a redirection, 303 is
    /// > the safest choice.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::redirect_307("/foo");
    /// ```
    #[inline]
    pub fn redirect_307<S>(target: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        Self {
            status_code: 307,
            headers: vec![("Location".into(), target.into())],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds a `Response` that redirects the user to another URL with a 302 status code. This
    /// semantically means a temporary redirect.
    ///
    /// The difference between 308 and 302 is that the client must keep the same method after
    /// the redirection. For example if the browser sends a POST request to `/foo` and that route
    /// returns a 308 redirection to `/bar`, then the browser will make a POST request to `/bar`.
    /// With a 302 redirection it would use a GET request instead.
    ///
    /// > **Note**: If you're uncertain about which status code to use for a redirection, 303 is
    /// > the safest choice.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::redirect_302("/bar");
    /// ```
    #[inline]
    pub fn redirect_308<S>(target: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        Self {
            status_code: 308,
            headers: vec![("Location".into(), target.into())],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds a 200 `Response` with data.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::from_data("application/octet-stream", vec![1, 2, 3, 4]);
    /// ```
    #[inline]
    pub fn from_data<C, D>(content_type: C, data: D) -> Self
    where
        C: Into<Cow<'static, str>>,
        D: Into<Vec<u8>>,
    {
        Self {
            status_code: 200,
            headers: vec![("Content-Type".into(), content_type.into())],
            data: ResponseBody::from_data(data),
            upgrade: None,
        }
    }

    /// Builds a 200 `Response` with the content of a file.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use rouille::Response;
    ///
    /// let file = File::open("image.png").unwrap();
    /// let response = Response::from_file("image/png", file);
    /// ```
    #[inline]
    pub fn from_file<C>(content_type: C, file: File) -> Self
    where
        C: Into<Cow<'static, str>>,
    {
        Self {
            status_code: 200,
            headers: vec![("Content-Type".into(), content_type.into())],
            data: ResponseBody::from_file(file),
            upgrade: None,
        }
    }

    /// Builds a response for a file, honoring a single `Range: bytes=...` request.
    ///
    /// Closed, open-ended, and suffix byte ranges produce a `206 Partial Content` response.
    /// Unsatisfiable ranges produce `416 Range Not Satisfiable`. Malformed ranges and multipart
    /// ranges are ignored and produce the same full response as a request without `Range`.
    /// `HEAD` requests advertise the full content length without returning a body.
    pub fn from_file_with_range<C>(
        request: &Request,
        content_type: C,
        mut file: File,
    ) -> io::Result<Self>
    where
        C: Into<Cow<'static, str>>,
    {
        let content_type = content_type.into();
        let file_len = file.metadata()?.len();
        let range = if request.method() == "GET" {
            request
                .header("Range")
                .map(|value| parse_byte_range(value, file_len))
        } else {
            None
        };

        match range {
            Some(ByteRange::Satisfiable { start, end }) => {
                let len = end - start + 1;
                file.seek(SeekFrom::Start(start))?;
                Ok(Self {
                    status_code: 206,
                    headers: vec![
                        ("Content-Type".into(), content_type),
                        ("Accept-Ranges".into(), "bytes".into()),
                        (
                            "Content-Range".into(),
                            format!("bytes {start}-{end}/{file_len}").into(),
                        ),
                    ],
                    data: ResponseBody::from_reader_and_size(
                        file.take(len),
                        response_body_size(len)?,
                    ),
                    upgrade: None,
                })
            }
            Some(ByteRange::Unsatisfiable) => Ok(Self {
                status_code: 416,
                headers: vec![
                    ("Content-Type".into(), content_type),
                    ("Accept-Ranges".into(), "bytes".into()),
                    ("Content-Range".into(), format!("bytes */{file_len}").into()),
                ],
                data: ResponseBody::empty(),
                upgrade: None,
            }),
            None | Some(ByteRange::Ignore) => {
                let len = response_body_size(file_len)?;
                let data = if request.method() == "HEAD" {
                    ResponseBody {
                        data: Box::new(io::empty()),
                        data_length: Some(len),
                    }
                } else {
                    ResponseBody::from_reader_and_size(file.take(file_len), len)
                };
                Ok(Self {
                    status_code: 200,
                    headers: vec![
                        ("Content-Type".into(), content_type),
                        ("Accept-Ranges".into(), "bytes".into()),
                    ],
                    data,
                    upgrade: None,
                })
            }
        }
    }

    /// Builds a `Response` that outputs HTML.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::html("<p>hello <strong>world</strong></p>");
    /// ```
    #[inline]
    pub fn html<D>(content: D) -> Self
    where
        D: Into<String>,
    {
        Self {
            status_code: 200,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            data: ResponseBody::from_string(content),
            upgrade: None,
        }
    }

    /// Builds a `Response` that outputs SVG.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::svg("<svg xmlns='http://www.w3.org/2000/svg'/>");
    /// ```
    #[inline]
    pub fn svg<D>(content: D) -> Self
    where
        D: Into<String>,
    {
        Self {
            status_code: 200,
            headers: vec![("Content-Type".into(), "image/svg+xml; charset=utf-8".into())],
            data: ResponseBody::from_string(content),
            upgrade: None,
        }
    }

    /// Builds a `Response` that outputs plain text.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::text("hello world");
    /// ```
    #[inline]
    pub fn text<S>(text: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            status_code: 200,
            headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
            data: ResponseBody::from_string(text),
            upgrade: None,
        }
    }

    /// Builds a `Response` that outputs JSON.
    ///
    /// # Example
    ///
    /// ```
    /// extern crate serde;
    /// #[macro_use] extern crate serde_derive;
    /// #[macro_use] extern crate rouille;
    /// use rouille::Response;
    /// # fn main() {
    ///
    /// #[derive(Serialize)]
    /// struct MyStruct {
    ///     field1: String,
    ///     field2: i32,
    /// }
    ///
    /// let response = Response::json(&MyStruct { field1: "hello".to_owned(), field2: 5 });
    /// // The Response will contain something like `{ field1: "hello", field2: 5 }`
    /// # }
    /// ```
    #[inline]
    pub fn json<T>(content: &T) -> Self
    where
        T: serde::Serialize,
    {
        let data = serde_json::to_string(content).unwrap();

        Self {
            status_code: 200,
            headers: vec![(
                "Content-Type".into(),
                "application/json; charset=utf-8".into(),
            )],
            data: ResponseBody::from_data(data),
            upgrade: None,
        }
    }

    /// Builds a `Response` that returns a `401 Not Authorized` status
    /// and a `WWW-Authenticate` header.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::basic_http_auth_login_required("realm");
    /// ```
    #[inline]
    pub fn basic_http_auth_login_required(realm: &str) -> Self {
        // TODO: escape the realm
        Self {
            status_code: 401,
            headers: vec![(
                "WWW-Authenticate".into(),
                format!("Basic realm=\"{realm}\"").into(),
            )],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds an empty `Response` with a 204 status code.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::empty_204();
    /// ```
    #[inline]
    pub fn empty_204() -> Self {
        Self {
            status_code: 204,
            headers: vec![],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds an empty `Response` with a 400 status code.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::empty_400();
    /// ```
    #[inline]
    pub fn empty_400() -> Self {
        Self {
            status_code: 400,
            headers: vec![],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds an empty `Response` with a 404 status code.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::empty_404();
    /// ```
    #[inline]
    pub fn empty_404() -> Self {
        Self {
            status_code: 404,
            headers: vec![],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Builds an empty `Response` with a 406 status code.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::empty_406();
    /// ```
    #[inline]
    pub fn empty_406() -> Self {
        Self {
            status_code: 406,
            headers: vec![],
            data: ResponseBody::empty(),
            upgrade: None,
        }
    }

    /// Changes the status code of the response.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::Response;
    /// let response = Response::text("hello world").with_status_code(500);
    /// ```
    #[inline]
    pub const fn with_status_code(mut self, code: u16) -> Self {
        self.status_code = code;
        self
    }

    /// Removes all headers from the response that match `header`.
    pub fn without_header(mut self, header: &str) -> Self {
        self.headers
            .retain(|(h, _)| !h.eq_ignore_ascii_case(header));
        self
    }

    /// Adds an additional header to the response.
    #[inline]
    pub fn with_additional_header<H, V>(mut self, header: H, value: V) -> Self
    where
        H: Into<Cow<'static, str>>,
        V: Into<Cow<'static, str>>,
    {
        self.headers.push((header.into(), value.into()));
        self
    }

    /// Removes all headers from the response whose names are `header`, and replaces them .
    pub fn with_unique_header<H, V>(mut self, header: H, value: V) -> Self
    where
        H: Into<Cow<'static, str>>,
        V: Into<Cow<'static, str>>,
    {
        // If Vec::retain provided a mutable reference this code would be much simpler and would
        // only need to iterate once.
        // See https://github.com/rust-lang/rust/issues/25477

        // TODO: if the response already has a matching header we shouldn't have to build a Cow
        // from the header

        let header = header.into();

        let mut found_one = false;
        self.headers.retain(|(h, _)| {
            if h.eq_ignore_ascii_case(&header) {
                if !found_one {
                    found_one = true;
                    true
                } else {
                    false
                }
            } else {
                true
            }
        });

        if found_one {
            for &mut (ref h, ref mut v) in &mut self.headers {
                if !h.eq_ignore_ascii_case(&header) {
                    continue;
                }
                *v = value.into();
                break;
            }
            self
        } else {
            self.with_additional_header(header, value)
        }
    }

    /// Adds or replaces a `ETag` header to the response, and turns the response into an empty 304
    /// response if the ETag matches a `If-None-Match` header of the request.
    ///
    /// An ETag is a unique representation of the content of a resource. If the content of the
    /// resource changes, the ETag should change as well.
    /// The purpose of using ETags is that a client can later ask the server to send the body of
    /// a response only if it still matches a certain ETag the client has stored in memory.
    ///
    /// > **Note**: You should always try to specify an ETag for responses that have a large body.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rouille::Request;
    /// use rouille::Response;
    ///
    /// fn handle(request: &Request) -> Response {
    ///     Response::text("hello world").with_etag(request, "my-etag-1234")
    /// }
    /// ```
    #[inline]
    pub fn with_etag<E>(self, request: &Request, etag: E) -> Self
    where
        E: Into<Cow<'static, str>>,
    {
        self.with_etag_keep(etag).simplify_if_etag_match(request)
    }

    /// Turns the response into an empty 304 response if the `ETag` that is stored in it matches a
    /// `If-None-Match` header of the request.
    pub fn simplify_if_etag_match(mut self, request: &Request) -> Self {
        if self.status_code < 200 || self.status_code >= 300 {
            return self;
        }

        let mut not_modified = false;
        for (key, etag) in &self.headers {
            if !key.eq_ignore_ascii_case("ETag") {
                continue;
            }

            not_modified = request
                .header("If-None-Match")
                .map(|header| header == etag)
                .unwrap_or(false);
        }

        if not_modified {
            self.data = ResponseBody::empty();
            self.status_code = 304;
        }

        self
    }

    /// Adds a `ETag` header to the response, or replaces an existing header if there is one.
    ///
    /// > **Note**: Contrary to `with_etag`, this function doesn't try to turn the response into
    /// > a 304 response. If you're unsure of what to do, prefer `with_etag`.
    #[inline]
    pub fn with_etag_keep<E>(self, etag: E) -> Self
    where
        E: Into<Cow<'static, str>>,
    {
        self.with_unique_header("ETag", etag)
    }

    /// Adds or replace a `Content-Disposition` header of the response. Tells the browser that the
    /// body of the request should fire a download popup instead of being shown in the browser.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rouille::Request;
    /// use rouille::Response;
    ///
    /// fn handle(request: &Request) -> Response {
    ///     Response::text("hello world").with_content_disposition_attachment("book.txt")
    /// }
    /// ```
    ///
    /// When the response is sent back to the browser, it will show a popup asking the user to
    /// download the file "book.txt" whose content will be "hello world".
    pub fn with_content_disposition_attachment(mut self, filename: &str) -> Self {
        // The name must be percent-encoded.
        let name = percent_encoding::percent_encode(filename.as_bytes(), super::DEFAULT_ENCODE_SET);

        // If you find a more elegant way to do the thing below, don't hesitate to open a PR

        // Support for this format varies browser by browser, so this may not be the most
        // ideal thing.
        // TODO: it's maybe possible to specify multiple file names
        let mut header = Some(format!("attachment; filename*=UTF8''{name}").into());

        for &mut (ref key, ref mut val) in &mut self.headers {
            if key.eq_ignore_ascii_case("Content-Disposition") {
                *val = header.take().unwrap();
                break;
            }
        }

        if let Some(header) = header {
            self.headers.push(("Content-Disposition".into(), header));
        }

        self
    }

    /// Adds or replaces a `Cache-Control` header that specifies that the resource is public and
    /// can be cached for the given number of seconds.
    ///
    /// > **Note**: This function doesn't do any caching itself. It just indicates that clients
    /// > that receive this response are allowed to cache it.
    #[inline]
    pub fn with_public_cache(self, max_age_seconds: u64) -> Self {
        self.with_unique_header(
            "Cache-Control",
            format!("public, max-age={max_age_seconds}"),
        )
        .without_header("Expires")
        .without_header("Pragma")
    }

    /// Adds or replaces a `Cache-Control` header that specifies that the resource is private and
    /// can be cached for the given number of seconds.
    ///
    /// Only the browser or the final client is authorized to cache the resource. Intermediate
    /// proxies must not cache it.
    ///
    /// > **Note**: This function doesn't do any caching itself. It just indicates that clients
    /// > that receive this response are allowed to cache it.
    #[inline]
    pub fn with_private_cache(self, max_age_seconds: u64) -> Self {
        self.with_unique_header(
            "Cache-Control",
            format!("private, max-age={max_age_seconds}"),
        )
        .without_header("Expires")
        .without_header("Pragma")
    }

    /// Adds or replaces a `Cache-Control` header that specifies that the client must not cache
    /// the resource.
    #[inline]
    pub fn with_no_cache(self) -> Self {
        self.with_unique_header("Cache-Control", "no-cache, no-store, must-revalidate")
            .with_unique_header("Expires", "0")
            .with_unique_header("Pragma", "no-cache")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ByteRange {
    Ignore,
    Unsatisfiable,
    Satisfiable { start: u64, end: u64 },
}

fn parse_byte_range(value: &str, file_len: u64) -> ByteRange {
    let Some((unit, value)) = value.split_once('=') else {
        return ByteRange::Ignore;
    };
    let value = value.trim();
    if !unit.trim().eq_ignore_ascii_case("bytes") || value.contains(',') {
        return ByteRange::Ignore;
    }

    let Some((first, last)) = value.split_once('-') else {
        return ByteRange::Ignore;
    };
    if last.contains('-') {
        return ByteRange::Ignore;
    }

    let first = first.trim();
    let last = last.trim();
    if first.is_empty() {
        let Ok(suffix_len) = last.parse::<u64>() else {
            return ByteRange::Ignore;
        };
        if suffix_len == 0 || file_len == 0 {
            return ByteRange::Unsatisfiable;
        }
        return ByteRange::Satisfiable {
            start: file_len.saturating_sub(suffix_len),
            end: file_len - 1,
        };
    }

    let Ok(start) = first.parse::<u64>() else {
        return ByteRange::Ignore;
    };
    let end = if last.is_empty() {
        file_len.saturating_sub(1)
    } else {
        let Ok(end) = last.parse::<u64>() else {
            return ByteRange::Ignore;
        };
        end
    };
    if file_len == 0 || start >= file_len || start > end {
        return ByteRange::Unsatisfiable;
    }

    ByteRange::Satisfiable {
        start,
        end: end.min(file_len - 1),
    }
}

fn response_body_size(len: u64) -> io::Result<usize> {
    usize::try_from(len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "response body length exceeds the platform limit",
        )
    })
}

/// An opaque type that represents the body of a response.
///
/// You can't access the inside of this struct, but you can build one by using one of the provided
/// constructors.
///
/// # Example
///
/// ```
/// use rouille::ResponseBody;
/// let body = ResponseBody::from_string("hello world");
/// ```
pub struct ResponseBody {
    data: Box<dyn Read + Send>,
    data_length: Option<usize>,
}

impl ResponseBody {
    /// Builds a `ResponseBody` that doesn't return any data.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::ResponseBody;
    /// let body = ResponseBody::empty();
    /// ```
    #[inline]
    pub fn empty() -> Self {
        Self {
            data: Box::new(io::empty()),
            data_length: Some(0),
        }
    }

    /// Builds a new `ResponseBody` that will read the data from a `Read`.
    ///
    /// Note that this is suboptimal compared to other constructors because the length
    /// isn't known in advance.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::io;
    /// use std::io::Read;
    /// use rouille::ResponseBody;
    ///
    /// let body = ResponseBody::from_reader(io::stdin().take(128));
    /// ```
    #[inline]
    pub fn from_reader<R>(data: R) -> Self
    where
        R: Read + Send + 'static,
    {
        Self {
            data: Box::new(data),
            data_length: None,
        }
    }

    /// Builds a new `ResponseBody` that will read the data from a `Read`.
    ///
    /// The caller must provide the content length. It is unspecified
    /// what will happen if the content length does not match the actual
    /// length of the data returned from the reader.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::io;
    /// use std::io::Read;
    /// use rouille::ResponseBody;
    ///
    /// let body = ResponseBody::from_reader_and_size(io::stdin().take(128), 128);
    /// ```
    #[inline]
    pub fn from_reader_and_size<R>(data: R, size: usize) -> Self
    where
        R: Read + Send + 'static,
    {
        Self {
            data: Box::new(data),
            data_length: Some(size),
        }
    }

    /// Builds a new `ResponseBody` that returns the given data.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::ResponseBody;
    /// let body = ResponseBody::from_data(vec![12u8, 97, 34]);
    /// ```
    #[inline]
    pub fn from_data<D>(data: D) -> Self
    where
        D: Into<Vec<u8>>,
    {
        let data = data.into();
        let len = data.len();

        Self {
            data: Box::new(Cursor::new(data)),
            data_length: Some(len),
        }
    }

    /// Builds a new `ResponseBody` that returns the content of the given file.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use rouille::ResponseBody;
    ///
    /// let file = File::open("page.html").unwrap();
    /// let body = ResponseBody::from_file(file);
    /// ```
    #[inline]
    pub fn from_file(file: File) -> Self {
        let len = file.metadata().map(|metadata| metadata.len() as usize).ok();

        Self {
            data: Box::new(file),
            data_length: len,
        }
    }

    /// Builds a new `ResponseBody` that returns an UTF-8 string.
    ///
    /// # Example
    ///
    /// ```
    /// use rouille::ResponseBody;
    /// let body = ResponseBody::from_string("hello world");
    /// ```
    #[inline]
    pub fn from_string<S>(data: S) -> Self
    where
        S: Into<String>,
    {
        Self::from_data(data.into().into_bytes())
    }

    /// Extracts the content of the response.
    ///
    /// Returns the size of the body and the body itself. If the size is `None`, then it is
    /// unknown.
    #[inline]
    pub fn into_reader_and_size(self) -> (Box<dyn Read + Send>, Option<usize>) {
        (self.data, self.data_length)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Request, Response};
    use std::{
        fs::File,
        io::{Read, Write},
        net::TcpStream,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    fn test_file(data: &[u8]) -> (File, PathBuf) {
        let id = TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rouille-range-test-{}-{id}.bin",
            std::process::id()
        ));
        std::fs::write(&path, data).unwrap();
        (File::open(&path).unwrap(), path)
    }

    fn request(method: &str, range: Option<&str>) -> Request {
        let headers = range
            .map(|value| vec![("Range".to_owned(), value.to_owned())])
            .unwrap_or_default();
        Request::fake_http(method, "/recording.mp4", headers, Vec::new())
    }

    fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_ref())
    }

    fn read_body(response: Response) -> (Option<usize>, Vec<u8>) {
        let (mut reader, len) = response.data.into_reader_and_size();
        let mut body = Vec::new();
        reader.read_to_end(&mut body).unwrap();
        (len, body)
    }

    fn file_response(method: &str, range: Option<&str>) -> (Response, PathBuf) {
        let (file, path) = test_file(b"0123456789");
        let response =
            Response::from_file_with_range(&request(method, range), "video/mp4", file).unwrap();
        (response, path)
    }

    fn remove_test_file(path: PathBuf) {
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unique_header_adds() {
        let r = Response {
            headers: vec![],
            ..Response::empty_400()
        };

        let r = r.with_unique_header("Foo", "Bar");

        assert_eq!(r.headers.len(), 1);
        assert_eq!(r.headers[0], ("Foo".into(), "Bar".into()));
    }

    #[test]
    fn unique_header_adds_without_touching() {
        let r = Response {
            headers: vec![("Bar".into(), "Foo".into())],
            ..Response::empty_400()
        };

        let r = r.with_unique_header("Foo", "Bar");

        assert_eq!(r.headers.len(), 2);
        assert_eq!(r.headers[0], ("Bar".into(), "Foo".into()));
        assert_eq!(r.headers[1], ("Foo".into(), "Bar".into()));
    }

    #[test]
    fn unique_header_replaces() {
        let r = Response {
            headers: vec![
                ("foo".into(), "A".into()),
                ("fOO".into(), "B".into()),
                ("Foo".into(), "C".into()),
            ],
            ..Response::empty_400()
        };

        let r = r.with_unique_header("Foo", "Bar");

        assert_eq!(r.headers.len(), 1);
        assert_eq!(r.headers[0], ("foo".into(), "Bar".into()));
    }

    #[test]
    fn file_without_range_returns_full_snapshot() {
        let (response, path) = file_response("GET", None);
        assert_eq!(response.status_code, 200);
        assert_eq!(header(&response, "Accept-Ranges"), Some("bytes"));
        let (len, body) = read_body(response);
        assert_eq!(len, Some(10));
        assert_eq!(body, b"0123456789");
        remove_test_file(path);
    }

    #[test]
    fn file_closed_range_returns_partial_content() {
        let (response, path) = file_response("GET", Some("bytes=2-5"));
        assert_eq!(response.status_code, 206);
        assert_eq!(header(&response, "Content-Range"), Some("bytes 2-5/10"));
        let (len, body) = read_body(response);
        assert_eq!(len, Some(4));
        assert_eq!(body, b"2345");
        remove_test_file(path);
    }

    #[test]
    fn file_open_ended_range_returns_to_end() {
        let (response, path) = file_response("GET", Some("bytes=7-"));
        assert_eq!(response.status_code, 206);
        assert_eq!(header(&response, "Content-Range"), Some("bytes 7-9/10"));
        let (len, body) = read_body(response);
        assert_eq!(len, Some(3));
        assert_eq!(body, b"789");
        remove_test_file(path);
    }

    #[test]
    fn file_suffix_range_returns_tail() {
        let (response, path) = file_response("GET", Some("bytes=-3"));
        assert_eq!(response.status_code, 206);
        assert_eq!(header(&response, "Content-Range"), Some("bytes 7-9/10"));
        let (len, body) = read_body(response);
        assert_eq!(len, Some(3));
        assert_eq!(body, b"789");
        remove_test_file(path);
    }

    #[test]
    fn file_range_end_is_clamped_to_snapshot() {
        let (response, path) = file_response("GET", Some("bytes=7-99"));
        assert_eq!(response.status_code, 206);
        assert_eq!(header(&response, "Content-Range"), Some("bytes 7-9/10"));
        let (len, body) = read_body(response);
        assert_eq!(len, Some(3));
        assert_eq!(body, b"789");
        remove_test_file(path);
    }

    #[test]
    fn file_unsatisfiable_range_returns_416() {
        let (response, path) = file_response("GET", Some("bytes=20-30"));
        assert_eq!(response.status_code, 416);
        assert_eq!(header(&response, "Content-Range"), Some("bytes */10"));
        let (len, body) = read_body(response);
        assert_eq!(len, Some(0));
        assert!(body.is_empty());
        remove_test_file(path);
    }

    #[test]
    fn malformed_and_multipart_ranges_are_ignored() {
        for range in ["items=0-1", "bytes=abc-def", "bytes=0-1,4-5"] {
            let (response, path) = file_response("GET", Some(range));
            assert_eq!(response.status_code, 200, "range: {range}");
            let (len, body) = read_body(response);
            assert_eq!(len, Some(10));
            assert_eq!(body, b"0123456789");
            remove_test_file(path);
        }
    }

    #[test]
    fn head_file_response_has_full_length_and_no_body() {
        let (response, path) = file_response("HEAD", Some("bytes=2-5"));
        assert_eq!(response.status_code, 200);
        assert_eq!(header(&response, "Accept-Ranges"), Some("bytes"));
        assert_eq!(header(&response, "Content-Range"), None);
        let (len, body) = read_body(response);
        assert_eq!(len, Some(10));
        assert!(body.is_empty());
        remove_test_file(path);
    }

    #[test]
    fn ranged_file_headers_reach_http_client() {
        let (_, path) = test_file(b"0123456789");
        let served_path = path.clone();
        let server = crate::Server::new("127.0.0.1:0", move |request| {
            let file = File::open(&served_path).unwrap();
            Response::from_file_with_range(request, "video/mp4", file).unwrap()
        })
        .unwrap();
        let address = server.server_addr();
        let (server_thread, stop_server) = server.stoppable();

        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(
                b"GET /recording.mp4 HTTP/1.1\r\nHost: localhost\r\nRange: bytes=2-5\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();

        stop_server.send(()).unwrap();
        server_thread.join().unwrap();
        remove_test_file(path);

        let (headers, body) = response.split_at(
            response
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .unwrap()
                + 4,
        );
        let headers = String::from_utf8(headers.to_vec())
            .unwrap()
            .to_ascii_lowercase();
        assert!(headers.starts_with("http/1.1 206 partial content\r\n"));
        assert!(headers.contains("accept-ranges: bytes\r\n"));
        assert!(headers.contains("content-range: bytes 2-5/10\r\n"));
        assert!(headers.contains("content-length: 4\r\n"));
        assert_eq!(body, b"2345");
    }
}

// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hand-rolled parser for RTSP/1.0 message heads.
//!
//! This parser is generic over [`Input`], supporting both contiguous
//! `&[u8]` (via [`Contiguous`](super::inputs::Contiguous)) and discontiguous ring-buffer views.

use super::{
    inputs::{Contiguous, Input, Slice as _},
    msg::{Data, HeaderName, HeaderValue, Headers, Message, Method, Request, Response, StatusCode},
    table::is_tchar,
};
use std::num::NonZeroUsize;
use url::Url;

/// Error detail carried by [`FeedError::Invalid`].
#[derive(Debug, Default, derive_more::Error)]
pub struct Invalid {
    /// Byte offset from the start of the stream where the error was detected.
    pub pos: u64,
    /// ABNF production names, innermost first.
    pub context: Vec<&'static str>,
    /// Underlying error from a library call, if any.
    #[error(source)]
    pub source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "byte {}: ", self.pos)?;
        if self.context.is_empty() {
            f.write_str("(no context)")?;
        } else {
            for (i, label) in self.context.iter().rev().enumerate() {
                if i > 0 {
                    f.write_str(" > ")?;
                }
                f.write_str(label)?;
            }
        }
        if let Some(ref src) = self.source {
            write!(f, ": {src}")?;
        }
        Ok(())
    }
}

/// Error returned by [`Parser::feed`].
#[derive(Debug, derive_more::Display, derive_more::Error)]
pub enum FeedError {
    /// Not enough data yet; call `feed` again after receiving more bytes.
    ///
    /// The `Option<NonZeroUsize>` holds the number of additional body bytes
    /// needed, if known. This is `Some` when the message head has been fully
    /// parsed and only the body remains; `None` during head parsing.
    #[display("incomplete RTSP message")]
    #[error(ignore)]
    Incomplete(Option<NonZeroUsize>),
    /// Malformed message; the connection should be dropped.
    #[display("invalid RTSP message: {_0}")]
    Invalid(Invalid),
}

impl FeedError {
    fn invalid() -> Self {
        Self::Invalid(Invalid::default())
    }

    fn map_invalid(self, f: impl FnOnce(&mut Invalid)) -> Self {
        if let Self::Invalid(mut inv) = self {
            f(&mut inv);
            Self::Invalid(inv)
        } else {
            self
        }
    }

    fn with_pos(self, pos: u64) -> Self {
        self.map_invalid(|inv| inv.pos = pos)
    }

    fn context(self, label: &'static str) -> Self {
        self.map_invalid(|inv| inv.context.push(label))
    }
}

fn invalid(label: &'static str) -> FeedError {
    FeedError::Invalid(Invalid {
        pos: 0,
        context: vec![label],
        source: None,
    })
}

fn invalid_err<E: std::error::Error + Send + Sync + 'static>(
    label: &'static str,
    e: E,
) -> FeedError {
    FeedError::Invalid(Invalid {
        pos: 0,
        context: vec![label],
        source: Some(Box::new(e)),
    })
}

fn incomplete_or_invalid<'i, I: Input<'i>>(input: &I) -> FeedError {
    if input.is_partial() {
        FeedError::Incomplete(None)
    } else {
        FeedError::invalid()
    }
}

fn line_exceeds_size_limit<'i, I: Input<'i>>(
    input: &I,
    start: usize,
    max_line_bytes: usize,
) -> bool {
    let remaining_bytes = input.len() - start;
    let bytes_to_check = remaining_bytes.min(max_line_bytes.saturating_add(1));
    for offset in 0..bytes_to_check {
        let byte = input.byte_at(start + offset);
        if byte == b'\r'
            && offset + 1 < remaining_bytes
            && input.byte_at(start + offset + 1) == b'\n'
        {
            return offset.saturating_add(2) > max_line_bytes;
        }
        if byte == b'\n' {
            return false;
        }
    }
    remaining_bytes > max_line_bytes
}

fn head_exceeds_size_limit<'i, I: Input<'i>>(input: &I, max_head_bytes: usize) -> bool {
    let mut start = 0;
    loop {
        if start > max_head_bytes {
            return true;
        }
        if start == input.len() {
            return false;
        }
        if input.byte_at(start) == b'$' {
            return false;
        }
        if input.byte_at(start) != b'\r' {
            return line_exceeds_size_limit(input, start, max_head_bytes.saturating_sub(start));
        }
        if start + 1 == input.len() {
            return input.len() > max_head_bytes;
        }
        if input.byte_at(start + 1) != b'\n' {
            return false;
        }
        start += 2;
    }
}

/// Builder for [`Parser`]. Obtain via [`Parser::builder`].
pub struct ParserBuilder {
    max_message_size: usize,
}

impl ParserBuilder {
    /// Sets the maximum total message size in bytes (head + body combined).
    ///
    /// Messages exceeding this limit are rejected with [`FeedError::Invalid`].
    /// Defaults to [`usize::MAX`] (no limit).
    pub const fn max_message_size(mut self, n: usize) -> Self {
        self.max_message_size = n;
        self
    }

    pub fn build(self) -> Parser {
        Parser {
            max_message_size: self.max_message_size,
            state: ParserState::default(),
            stream_pos: 0,
            msg_start_pos: 0,
        }
    }
}

pub struct Parser {
    max_message_size: usize,
    state: ParserState,
    /// Cumulative bytes consumed from the stream across all `feed` calls.
    stream_pos: u64,
    /// Stream offset where the current message begins, past any leading blank lines.
    msg_start_pos: u64,
}

impl Default for Parser {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[derive(Default)]
enum ParserState {
    #[default]
    Idle,
    /// Start of a first line that an interleaved frame split before its LF.
    FirstLine {
        pending: Vec<u8>,
    },
    /// Accumulating headers for a request or response.
    Head {
        msg: Message,
        head_bytes: usize,
        /// Start of a header line that an interleaved frame split before its LF.
        pending: Vec<u8>,
    },
    Body(Message, usize),
}

enum Step<'i, I: Input<'i>> {
    Continue,
    Output(Option<(Message, I::Slice)>),
}

impl Parser {
    pub const fn builder() -> ParserBuilder {
        ParserBuilder {
            max_message_size: usize::MAX,
        }
    }

    pub const fn stream_pos(&self) -> u64 {
        self.stream_pos
    }

    /// Returns the stream offset where the current message began, past any leading blank lines.
    pub const fn msg_start_pos(&self) -> u64 {
        self.msg_start_pos
    }

    /// Parses the next message from `input`.
    ///
    /// Returns `Ok(Some(...))` when a complete message has been parsed.
    /// Returns `Ok(None)` when idle between messages with no remaining input;
    /// leading blank lines may have been consumed, and callers must advance their
    /// buffer by the bytes consumed from `input`.
    /// On `Err(Incomplete(_))`, callers must advance past stably consumed bytes
    /// and call again when more data arrives.
    /// On `Err(Invalid)`, the connection is unrecoverable; [`Invalid::pos`]
    /// gives the stream offset where the error was detected.
    pub fn feed<'i, I: Input<'i>>(
        &mut self,
        input: &mut I,
    ) -> Result<Option<(Message, I::Slice)>, FeedError> {
        let start_pos = self.stream_pos;
        let initial_len = input.len();
        let result = self.feed_inner(input, start_pos, initial_len);
        let consumed = (initial_len - input.len()) as u64;
        self.stream_pos += consumed;
        result.map_err(|e| e.with_pos(start_pos + consumed))
    }

    fn feed_inner<'i, I: Input<'i>>(
        &mut self,
        input: &mut I,
        start_pos: u64,
        initial_len: usize,
    ) -> Result<Option<(Message, I::Slice)>, FeedError> {
        loop {
            let step = match std::mem::take(&mut self.state) {
                ParserState::Idle => self.feed_idle(input, start_pos, initial_len)?,
                ParserState::FirstLine { pending } => self.feed_first_line(input, pending)?,
                ParserState::Head {
                    msg,
                    head_bytes,
                    pending,
                } => self.feed_head(input, msg, head_bytes, pending)?,
                ParserState::Body(msg, body_len) => {
                    if input.len() >= body_len {
                        let body = input.next_slice(body_len);
                        Step::Output(Some((msg, body)))
                    } else {
                        self.state = ParserState::Body(msg, body_len);
                        let needed = body_len - input.len();
                        return Err(FeedError::Incomplete(NonZeroUsize::new(needed)));
                    }
                }
            };
            if let Step::Output(output) = step {
                return Ok(output);
            }
        }
    }

    fn feed_idle<'i, I: Input<'i>>(
        &mut self,
        input: &mut I,
        start_pos: u64,
        initial_len: usize,
    ) -> Result<Step<'i, I>, FeedError> {
        // Step over blank lines between messages so `msg_start_pos` points at the
        // first byte of the message itself.
        while input.len() >= 2 && input.peek_byte() == Some(b'\r') && input.byte_at(1) == b'\n' {
            input.advance(2);
        }
        self.msg_start_pos = start_pos + (initial_len - input.len()) as u64;
        let checkpoint = *input;
        let before = input.len();
        match first_line_frame(input, self.max_message_size) {
            Ok(Some((text_len, channel_id, body_len))) => {
                let pending = input.next_slice(text_len).to_cow().into_owned();
                input.advance(4);
                let body = input.next_slice(usize::from(body_len));
                self.state = ParserState::FirstLine { pending };
                let data = Data {
                    channel_id,
                    body_len,
                };
                return Ok(Step::Output(Some((Message::Data(data), body))));
            }
            Ok(None) => {}
            Err(FeedError::Incomplete(_)) => {
                *input = checkpoint;
                return Err(FeedError::Incomplete(None));
            }
            Err(e) => return Err(e),
        }
        if self.max_message_size != usize::MAX
            && head_exceeds_size_limit(input, self.max_message_size)
        {
            return Err(invalid("message-too-large"));
        }
        match parse_head(input) {
            Ok(None) => Ok(Step::Output(None)),
            Ok(Some(Message::Data(data))) => {
                let body_len = data.body_len as usize;
                self.state = ParserState::Body(Message::Data(data), body_len);
                Ok(Step::Continue)
            }
            Ok(Some(msg)) => {
                let head_bytes = before - input.len();
                if head_bytes > self.max_message_size {
                    return Err(invalid("message-too-large"));
                }
                self.state = ParserState::Head {
                    msg,
                    head_bytes,
                    pending: Vec::new(),
                };
                Ok(Step::Continue)
            }
            Err(FeedError::Incomplete(_)) => {
                *input = checkpoint;
                Err(FeedError::Incomplete(None))
            }
            Err(e) => Err(e),
        }
    }

    fn feed_first_line<'i, I: Input<'i>>(
        &mut self,
        input: &mut I,
        mut pending: Vec<u8>,
    ) -> Result<Step<'i, I>, FeedError> {
        let max_text = self.max_message_size.saturating_sub(pending.len());
        match embedded_frame(input, max_text) {
            Ok(Some((text_len, channel_id, body_len))) => {
                pending.extend_from_slice(&input.next_slice(text_len).to_cow());
                input.advance(4);
                let body = input.next_slice(usize::from(body_len));
                self.state = ParserState::FirstLine { pending };
                let data = Data {
                    channel_id,
                    body_len,
                };
                return Ok(Step::Output(Some((Message::Data(data), body))));
            }
            Ok(None) => {}
            Err(e) => {
                self.state = ParserState::FirstLine { pending };
                return Err(e);
            }
        }
        let Some(end) = input.find_byte(b'\n') else {
            if pending.len().saturating_add(input.len()) > self.max_message_size {
                return Err(invalid("message-too-large"));
            }
            self.state = ParserState::FirstLine { pending };
            return Err(incomplete_or_invalid(input));
        };
        let line_len = pending.len().saturating_add(end + 1);
        if line_len > self.max_message_size {
            return Err(invalid("message-too-large"));
        }
        pending.extend_from_slice(&input.next_slice(end + 1).to_cow());
        let mut complete = Contiguous::new(&pending, false);
        let msg = match parse_head(&mut complete)? {
            Some(msg @ (Message::Request(_) | Message::Response(_))) => msg,
            // `pending` starts with neither CRLF nor `$`.
            _ => unreachable!("reconstructed first line must parse as request or response"),
        };
        self.state = ParserState::Head {
            msg,
            head_bytes: pending.len(),
            pending: Vec::new(),
        };
        Ok(Step::Continue)
    }

    fn feed_head<'i, I: Input<'i>>(
        &mut self,
        input: &mut I,
        msg: Message,
        mut head_bytes: usize,
        mut pending: Vec<u8>,
    ) -> Result<Step<'i, I>, FeedError> {
        let context = match &msg {
            Message::Request(_) => "request",
            Message::Response(_) => "response",
            Message::Data(_) => unreachable!("data messages do not have headers"),
        };
        let max_remaining = self.max_message_size.saturating_sub(head_bytes);
        match embedded_frame(input, max_remaining) {
            Ok(Some((text_len, channel_id, body_len))) => {
                pending.extend_from_slice(&input.next_slice(text_len).to_cow());
                head_bytes += text_len;
                input.advance(4);
                let body = input.next_slice(usize::from(body_len));
                self.state = ParserState::Head {
                    msg,
                    head_bytes,
                    pending,
                };
                let data = Data {
                    channel_id,
                    body_len,
                };
                return Ok(Step::Output(Some((Message::Data(data), body))));
            }
            Ok(None) => {}
            Err(e) => {
                self.state = ParserState::Head {
                    msg,
                    head_bytes,
                    pending,
                };
                return Err(e.context(context));
            }
        }
        if self.max_message_size != usize::MAX && line_exceeds_size_limit(input, 0, max_remaining) {
            return Err(invalid("message-too-large").context(context));
        }
        self.finish_head_line(input, msg, head_bytes, pending, context)
    }

    fn finish_head_line<'i, I: Input<'i>>(
        &mut self,
        input: &mut I,
        mut msg: Message,
        mut head_bytes: usize,
        mut pending: Vec<u8>,
        context: &'static str,
    ) -> Result<Step<'i, I>, FeedError> {
        let checkpoint = *input;
        let before = input.len();
        let parsed = if pending.is_empty() {
            parse_header_line(input)
        } else {
            rest_of_line(input, &mut pending)
        };
        match parsed {
            Ok(Some((name, value))) => {
                head_bytes += before - input.len();
                if head_bytes > self.max_message_size {
                    return Err(invalid("message-too-large").context(context));
                }
                match &mut msg {
                    Message::Request(req) => req.headers.append(name, value),
                    Message::Response(resp) => resp.headers.append(name, value),
                    Message::Data(_) => unreachable!("data messages do not have headers"),
                }
                self.state = ParserState::Head {
                    msg,
                    head_bytes,
                    pending,
                };
                Ok(Step::Continue)
            }
            Ok(None) => {
                head_bytes += before - input.len();
                let headers = match &msg {
                    Message::Request(req) => &req.headers,
                    Message::Response(resp) => &resp.headers,
                    Message::Data(_) => unreachable!("data messages do not have headers"),
                };
                let body_len = content_length(headers).map_err(|e| e.context(context))?;
                let total = (head_bytes as u64).saturating_add(body_len);
                if total > self.max_message_size as u64 {
                    return Err(invalid("message-too-large").context(context));
                }
                self.state = ParserState::Body(msg, body_len as usize);
                Ok(Step::Continue)
            }
            Err(FeedError::Incomplete(_)) => {
                *input = checkpoint;
                self.state = ParserState::Head {
                    msg,
                    head_bytes,
                    pending,
                };
                Err(FeedError::Incomplete(None))
            }
            Err(e) => {
                self.state = ParserState::Head {
                    msg,
                    head_bytes,
                    pending,
                };
                Err(e.context(context))
            }
        }
    }
}

/// Parses the first line of the next message.
/// Returns `Ok(None)` when the input is empty.
fn parse_head<'i, I: Input<'i>>(input: &mut I) -> Result<Option<Message>, FeedError> {
    match input.peek_byte() {
        None => Ok(None),
        Some(b'\r') => {
            if input.len() < 2 {
                return Err(incomplete_or_invalid(input));
            }
            Err(FeedError::invalid())
        }
        Some(b'$') => {
            input.advance(1);
            Ok(Some(Message::Data(parse_data(input)?)))
        }
        Some(_) => {
            if input.len() >= 5 && input.starts_with_lit(b"RTSP/") {
                Ok(Some(Message::Response(parse_status_line(input)?)))
            } else if input.len() < 5
                && input.is_partial()
                && input.starts_with_lit(&b"RTSP/"[..input.len()])
            {
                Err(FeedError::Incomplete(None))
            } else {
                Ok(Some(Message::Request(parse_request_line(input)?)))
            }
        }
    }
}

/// Finds an interleaved frame embedded inside the first line of a request or response.
fn first_line_frame<'i, I: Input<'i>>(
    input: &I,
    max_text_bytes: usize,
) -> Result<Option<(usize, u8, u16)>, FeedError> {
    match input.peek_byte() {
        None | Some(b'$') => Ok(None),
        Some(b'\r') => {
            if input.len() < 2 {
                return Err(incomplete_or_invalid(input));
            }
            Err(FeedError::invalid())
        }
        Some(_) => embedded_frame(input, max_text_bytes),
    }
}

/// Finds an interleaved frame embedded inside the current RTSP line before its LF.
///
/// Some cameras write interleaved RTP frames into the TCP stream while an RTSP
/// response is still being serialized. A `$` starts a frame only when the channel
/// byte is a control character other than HT, LF, or CR, which header text forbids.
fn embedded_frame<'i, I: Input<'i>>(
    input: &I,
    max_text_bytes: usize,
) -> Result<Option<(usize, u8, u16)>, FeedError> {
    let Some(at) = input.find_bytes2(b'$', b'\n') else {
        return Ok(None);
    };
    if input.byte_at(at) != b'$' {
        return Ok(None);
    }
    if input.len() < at + 2 {
        if at > max_text_bytes {
            return Err(invalid("message-too-large"));
        }
        return if input.is_partial() {
            Err(FeedError::Incomplete(None))
        } else {
            Ok(None)
        };
    }
    let channel_id = input.byte_at(at + 1);
    if channel_id >= 0x20 || matches!(channel_id, b'\t' | b'\n' | b'\r') {
        return Ok(None);
    }
    if at > max_text_bytes {
        return Err(invalid("message-too-large"));
    }
    if input.len() < at + 4 {
        return if input.is_partial() {
            Err(FeedError::Incomplete(None))
        } else {
            Ok(None)
        };
    }
    let body_len = u16::from(input.byte_at(at + 2)) << 8 | u16::from(input.byte_at(at + 3));
    if input.len() < at + 4 + usize::from(body_len) {
        return if input.is_partial() {
            Err(FeedError::Incomplete(None))
        } else {
            Ok(None)
        };
    }
    Ok(Some((at, channel_id, body_len)))
}

/// Parses the rest of a header line split by an embedded frame, together with `pending`.
fn rest_of_line<'i, I: Input<'i>>(
    input: &mut I,
    pending: &mut Vec<u8>,
) -> Result<Option<(HeaderName, HeaderValue)>, FeedError> {
    let Some(end) = input.find_byte(b'\n') else {
        return Err(incomplete_or_invalid(input));
    };
    let mut line = std::mem::take(pending);
    line.extend_from_slice(&input.next_slice(end + 1).to_cow());
    let mut complete = Contiguous::new(&line, false);
    parse_header_line(&mut complete)
}

fn parse_data<'i, I: Input<'i>>(input: &mut I) -> Result<Data, FeedError> {
    if input.len() < 3 {
        return Err(incomplete_or_invalid(input).context("interleaved-data"));
    }
    let channel_id = input.byte_at(0);
    let body_len = (input.byte_at(1) as u16) << 8 | input.byte_at(2) as u16;
    input.advance(3);
    Ok(Data {
        channel_id,
        body_len,
    })
}

fn parse_status_line<'i, I: Input<'i>>(input: &mut I) -> Result<Response, FeedError> {
    parse_status_line_inner(input).map_err(|e| e.context("status-line"))
}

fn parse_status_line_inner<'i, I: Input<'i>>(input: &mut I) -> Result<Response, FeedError> {
    eat_literal(input, b"RTSP/1.0 ").map_err(|e| e.context("RTSP-Version"))?;
    if input.len() < 3 {
        for i in 0..input.len() {
            if !input.byte_at(i).is_ascii_digit() {
                return Err(invalid("status-code"));
            }
        }
        return Err(incomplete_or_invalid(input).context("status-code"));
    }
    let b0 = input.byte_at(0);
    let b1 = input.byte_at(1);
    let b2 = input.byte_at(2);
    if !b0.is_ascii_digit() || !b1.is_ascii_digit() || !b2.is_ascii_digit() {
        return Err(invalid("status-code"));
    }
    input.advance(3);
    let code = (b0 - b'0') as u16 * 100 + (b1 - b'0') as u16 * 10 + (b2 - b'0') as u16;
    let status_code = StatusCode::try_from(code).map_err(|e| invalid_err("status-code", e))?;
    eat_byte(input, b' ')?;
    let reason_slice = take_line(input).map_err(|e| e.context("reason-phrase"))?;
    let reason_phrase = reason_slice
        .to_cow_str()
        .map_err(|e| invalid_err("reason-phrase", e))?
        .into_owned();
    Ok(Response {
        status_code,
        reason_phrase,
        headers: Headers::default(),
    })
}

fn parse_request_line<'i, I: Input<'i>>(input: &mut I) -> Result<Request, FeedError> {
    parse_request_line_inner(input).map_err(|e| e.context("request-line"))
}

fn parse_request_line_inner<'i, I: Input<'i>>(input: &mut I) -> Result<Request, FeedError> {
    let method_slice = take_token(input).map_err(|e| e.context("method"))?;
    let method = Method::try_from(method_slice.to_cow().into_owned())
        .map_err(|e| invalid_err("method", e))?;
    eat_byte(input, b' ')?;
    let request_uri = if input.peek_byte() == Some(b'*') {
        input.advance(1);
        None
    } else {
        let uri_str = take_vchar(input)
            .map_err(|e| e.context("Request-URI"))?
            .to_cow_str()
            .map_err(|e| invalid_err("Request-URI", e))?;
        if uri_str.is_empty() {
            return Err(invalid("Request-URI"));
        }
        Some(Url::parse(&uri_str).map_err(|e| invalid_err("Request-URI", e))?)
    };
    eat_literal(input, b" RTSP/1.0\r\n").map_err(|e| e.context("RTSP-Version"))?;
    Ok(Request {
        method,
        request_uri,
        headers: Headers::default(),
    })
}

fn parse_header_line<'i, I: Input<'i>>(
    input: &mut I,
) -> Result<Option<(HeaderName, HeaderValue)>, FeedError> {
    match input.peek_byte() {
        None => Err(incomplete_or_invalid(input)),
        Some(b'\r') => {
            if input.len() < 2 {
                return Err(incomplete_or_invalid(input));
            }
            if input.byte_at(1) != b'\n' {
                return Err(FeedError::invalid());
            }
            input.advance(2);
            Ok(None)
        }
        Some(_) => Ok(Some(parse_header_pair(input)?)),
    }
}

fn parse_header_pair<'i, I: Input<'i>>(
    input: &mut I,
) -> Result<(HeaderName, HeaderValue), FeedError> {
    parse_header_pair_inner(input).map_err(|e| e.context("message-header"))
}

fn parse_header_pair_inner<'i, I: Input<'i>>(
    input: &mut I,
) -> Result<(HeaderName, HeaderValue), FeedError> {
    let colon_pos = match input.find_bytes3(b':', b'\r', b'\n') {
        None => return Err(incomplete_or_invalid(input).context("field-name")),
        Some(n) => n,
    };
    if colon_pos == 0 || input.byte_at(colon_pos) != b':' {
        return Err(invalid("field-name"));
    }
    let name_slice = input.next_slice(colon_pos);
    input.advance(1); // consume ':'

    // Skip OWS after colon.
    match input.find_first(|b| !matches!(b, b' ' | b'\t')) {
        Some(n) => input.advance(n),
        None => return Err(incomplete_or_invalid(input).context("field-value")),
    }

    let crlf_pos = match input.find_bytes2(b'\r', b'\n') {
        None => return Err(incomplete_or_invalid(input).context("field-value")),
        Some(n) => n,
    };
    if input.byte_at(crlf_pos) != b'\r' {
        return Err(invalid("field-value"));
    }
    if input.len() <= crlf_pos + 1 {
        return Err(incomplete_or_invalid(input).context("field-value"));
    }
    if input.byte_at(crlf_pos + 1) != b'\n' {
        return Err(invalid("field-value"));
    }
    let value_slice = input.next_slice(crlf_pos);
    input.advance(2); // consume '\r\n'

    // Strip trailing OWS.
    let value_cow = value_slice.to_cow();
    let trimmed_len = value_cow.len()
        - value_cow
            .iter()
            .rev()
            .take_while(|&&b| b == b' ' || b == b'\t')
            .count();
    if trimmed_len == 0 {
        return Err(invalid("field-value"));
    }

    let name = HeaderName::try_from(name_slice.to_cow().into_owned())
        .map_err(|e| invalid_err("field-name", e))?;
    let value = HeaderValue::try_from(value_cow[..trimmed_len].to_vec())
        .map_err(|e| invalid_err("field-value", e))?;
    Ok((name, value))
}

fn content_length(headers: &Headers) -> Result<u64, FeedError> {
    headers.get("Content-Length").map_or_else(
        || Ok(0),
        |value| {
            value
                .parse::<u64>()
                .map_err(|error| invalid_err("Content-Length", error))
        },
    )
}

// ---------------------------------------------------------------------------
// Primitive helpers
// ---------------------------------------------------------------------------

fn take_line<'i, I: Input<'i>>(input: &mut I) -> Result<I::Slice, FeedError> {
    let cr_pos = match input.find_byte(b'\r') {
        None => return Err(incomplete_or_invalid(input)),
        Some(n) => n,
    };
    if input.len() <= cr_pos + 1 {
        return Err(incomplete_or_invalid(input));
    }
    if input.byte_at(cr_pos + 1) != b'\n' {
        return Err(FeedError::invalid());
    }
    let line = input.next_slice(cr_pos);
    input.advance(2); // consume '\r\n'
    Ok(line)
}

fn take_token<'i, I: Input<'i>>(input: &mut I) -> Result<I::Slice, FeedError> {
    let len = input.len();
    match input.find_first(|b| !is_tchar(b)) {
        Some(0) => Err(FeedError::invalid()),
        Some(n) => Ok(input.next_slice(n)),
        None if len == 0 => Err(incomplete_or_invalid(input)),
        None if input.is_partial() => Err(FeedError::Incomplete(None)),
        None => Ok(input.next_slice(len)),
    }
}

fn take_vchar<'i, I: Input<'i>>(input: &mut I) -> Result<I::Slice, FeedError> {
    let len = input.len();
    match input.find_first(|b| !matches!(b, 0x21..=0x7E)) {
        Some(n) => Ok(input.next_slice(n)),
        None if input.is_partial() => Err(FeedError::Incomplete(None)),
        None => Ok(input.next_slice(len)),
    }
}

fn eat_literal<'i, I: Input<'i>>(input: &mut I, lit: &[u8]) -> Result<(), FeedError> {
    if input.len() < lit.len() {
        if !input.starts_with_lit(&lit[..input.len()]) {
            return Err(FeedError::invalid());
        }
        return Err(incomplete_or_invalid(input));
    }
    if !input.starts_with_lit(lit) {
        return Err(FeedError::invalid());
    }
    input.advance(lit.len());
    Ok(())
}

fn eat_byte<'i, I: Input<'i>>(input: &mut I, b: u8) -> Result<(), FeedError> {
    match input.peek_byte() {
        None => Err(incomplete_or_invalid(input)),
        Some(x) if x != b => Err(FeedError::invalid()),
        Some(_) => {
            input.advance(1);
            Ok(())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::rtsp::inputs::Split;

    /// Exposed for `inputs::tests` to test `take_line` with different `Input` impls.
    pub fn take_line_for_test<'i, I: Input<'i>>(input: &mut I) -> Result<I::Slice, FeedError> {
        take_line(input)
    }

    /// Simulates two feed calls: partial first, then the remainder.
    fn two_feed<'a>(
        parser: &mut Parser,
        first: &'a [u8],
        second: &'a [u8],
    ) -> Result<Option<(Message, Split<'a>)>, FeedError> {
        let mut p1 = Split::new(first, &[], true);
        match parser.feed(&mut p1) {
            Ok(None) | Err(FeedError::Incomplete(_)) => {
                let consumed = first.len() - p1.len();
                let mut p2 = Split::new(&first[consumed..], second, false);
                parser.feed(&mut p2)
            }
            Ok(Some(r)) => Ok(Some(r)),
            Err(e) => Err(e),
        }
    }

    #[test]
    fn test_take_line() {
        let data = &b"foo\r\nbar"[..];
        for split in 0..data.len() {
            let (first, second) = data.split_at(split);
            let mut input = Split::new(first, second, false);
            match take_line(&mut input) {
                Err(e) => panic!("failed at split point {split}: {e:?}"),
                Ok(line) => {
                    assert_eq!(
                        line.to_cow().as_ref(),
                        b"foo",
                        "failed at split point {split}"
                    );
                }
            }
        }
    }

    #[test]
    fn parse_request() {
        let data = include_bytes!("testdata/request.txt");
        let expected = Request {
            method: Method::try_from("DESCRIBE").unwrap(),
            request_uri: Some(Url::parse("rtsp://192.168.5.225/h264Preview_01_main").unwrap()),
            headers: [
                (
                    HeaderName::try_from("Accept").unwrap(),
                    "application/sdp".try_into().unwrap(),
                ),
                (
                    HeaderName::try_from("CSeq").unwrap(),
                    "1".try_into().unwrap(),
                ),
                (
                    HeaderName::try_from("User-Agent").unwrap(),
                    "Retina mp4 example".try_into().unwrap(),
                ),
            ]
            .into(),
        };
        for split in 0..data.len() {
            let (first, second) = data.split_at(split);

            // Discontiguous ring-buffer view, single feed.
            let mut input = Split::new(first, second, false);
            let (Message::Request(_), body) = Parser::default()
                .feed(&mut input)
                .unwrap_or_else(|e| panic!("ring-buf failed at split {split}: {e:?}"))
                .unwrap_or_else(|| panic!("unexpected EOF at split {split}"))
            else {
                panic!()
            };
            assert!(body.to_cow().is_empty(), "split {split}");

            // Two separate feeds: partial first, complete second.
            let (Message::Request(_), body) = two_feed(&mut Parser::default(), first, second)
                .unwrap_or_else(|e| panic!("two-feed failed at split {split}: {e:?}"))
                .unwrap_or_else(|| panic!("two-feed unexpected EOF at split {split}"))
            else {
                panic!()
            };
            assert!(body.to_cow().is_empty(), "split {split}");
        }
        // Verify the parsed content matches the expected request (spot-check one split).
        let mut input = Split::new(data, &[], false);
        let (Message::Request(req), _) = Parser::default().feed(&mut input).unwrap().unwrap()
        else {
            panic!()
        };
        assert_eq!(req, expected);
    }

    #[test]
    fn parse_response() {
        let data = include_bytes!("testdata/response.txt");
        for split in 0..data.len() {
            let (first, second) = data.split_at(split);

            // Discontiguous ring-buffer view, single feed.
            let mut input = Split::new(first, second, false);
            Parser::default()
                .feed(&mut input)
                .unwrap_or_else(|e| panic!("ring-buf failed at split {split}: {e:?}"))
                .unwrap_or_else(|| panic!("unexpected EOF at split {split}"));

            // Two separate feeds: partial first, complete second.
            two_feed(&mut Parser::default(), first, second)
                .unwrap_or_else(|e| panic!("two-feed failed at split {split}: {e:?}"))
                .unwrap_or_else(|| panic!("two-feed unexpected EOF at split {split}"));
        }
    }

    #[test]
    fn parse_data() {
        let data = &b"\r\n$\x03\x00\x0512345"[..];
        for split in 0..data.len() {
            let (first, second) = data.split_at(split);

            // Discontiguous ring-buffer view, single feed.
            let mut input = Split::new(first, second, false);
            let (Message::Data(d), body) = Parser::default()
                .feed(&mut input)
                .unwrap_or_else(|e| panic!("ring-buf failed at split {split}: {e:?}"))
                .unwrap_or_else(|| panic!("unexpected EOF at split {split}"))
            else {
                panic!()
            };
            assert!(
                matches!(
                    d,
                    Data {
                        channel_id: 3,
                        body_len: 5
                    }
                ),
                "split {split}"
            );
            assert_eq!(body.to_cow().as_ref(), b"12345", "split {split}");

            // Two separate feeds: partial first, complete second.
            let (Message::Data(d), body) = two_feed(&mut Parser::default(), first, second)
                .unwrap_or_else(|e| panic!("two-feed failed at split {split}: {e:?}"))
                .unwrap_or_else(|| panic!("two-feed unexpected EOF at split {split}"))
            else {
                panic!()
            };
            assert!(
                matches!(
                    d,
                    Data {
                        channel_id: 3,
                        body_len: 5
                    }
                ),
                "split {split}"
            );
            assert_eq!(body.to_cow().as_ref(), b"12345", "split {split}");
        }
    }

    #[test]
    fn clean_eof() {
        let mut input = Split::new(&[], &[], false);
        let result = Parser::default().feed(&mut input);
        assert!(
            matches!(result, Ok(None)),
            "expected Ok(None) for clean EOF, got {result:?}",
        );
    }

    #[test]
    fn error_display_rtsp_version_mismatch() {
        let data = b"RTSP/2.0 200 OK\r\n\r\n";
        let mut input = Split::new(data, &[], false);
        let err = Parser::default().feed(&mut input).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid RTSP message: byte 0: status-line > RTSP-Version",
        );
    }

    #[test]
    fn error_display_bad_url() {
        let data = b"DESCRIBE not-a-url RTSP/1.0\r\n\r\n";
        let mut input = Split::new(data, &[], false);
        let err = Parser::default().feed(&mut input).unwrap_err();
        let FeedError::Invalid(ref inv) = err else {
            panic!("expected Invalid, got {err:?}");
        };
        assert_eq!(inv.pos, 18, "wrong position; full error: {err}");
        assert!(
            inv.context == ["Request-URI", "request-line"],
            "wrong context: {:?}",
            inv.context
        );
        assert!(inv.source.is_some(), "expected URL parse error as source");
    }

    #[test]
    fn max_message_size() {
        let data = b"GET * RTSP/1.0\r\nContent-Length: 5\r\n\r\nhello";
        let total = data.len();

        let mut input = Split::new(data, &[], false);
        Parser::builder()
            .max_message_size(total)
            .build()
            .feed(&mut input)
            .expect("should succeed at exact limit")
            .expect("unexpected EOF");

        let mut input = Split::new(data, &[], false);
        let err = Parser::builder()
            .max_message_size(total - 1)
            .build()
            .feed(&mut input)
            .unwrap_err();
        let FeedError::Invalid(ref inv) = err else {
            panic!("expected Invalid, got {err:?}");
        };
        assert_eq!(inv.context, ["message-too-large", "request"]);
    }

    #[test]
    fn max_message_size_rejects_incomplete_message_heads() {
        let start_line = b"RTSP/1.0 200 OK";
        let partial_header = b"RTSP/1.0 200 OK\r\nX: 0123456789";
        let complete_headers_without_terminator = b"RTSP/1.0 200 OK\r\nX: y\r\nX: y\r\nX: y\r\n";

        for (data, max_message_size) in [
            (start_line.as_slice(), start_line.len() - 1),
            (partial_header.as_slice(), 24),
            (complete_headers_without_terminator.as_slice(), 30),
        ] {
            let mut input = Split::new(data, &[], true);
            let result = Parser::builder()
                .max_message_size(max_message_size)
                .build()
                .feed(&mut input);
            assert!(
                matches!(result, Err(FeedError::Invalid(_))),
                "expected oversized partial message to fail, got {result:?}"
            );
        }
    }

    #[test]
    fn fail_eagerly() {
        let data = b"A b";
        let mut input = Split::new(data, &[], false);
        let Err(FeedError::Invalid(inv)) = Parser::builder().build().feed(&mut input) else {
            panic!();
        };
        let msg = inv.to_string();
        assert!(
            msg.contains("Request-URI"),
            "expected Request-URI failure, got {msg}"
        );
    }

    #[test]
    fn incomplete_body_reports_needed() {
        // Parse a response with Content-Length: 100 but only supply the head.
        let data = b"RTSP/1.0 200 OK\r\nContent-Length: 100\r\n\r\n";
        let mut input = Split::new(data, &[], true);
        let mut parser = Parser::default();
        let err = parser.feed(&mut input).unwrap_err();
        match err {
            FeedError::Incomplete(Some(n)) => assert_eq!(n.get(), 100),
            other => panic!("expected Incomplete(Some(100)), got {other:?}"),
        }
    }

    #[test]
    fn incomplete_data_body_reports_needed() {
        // Interleaved data: $\x00\x00\x05 but no body bytes.
        let data = b"$\x00\x00\x05";
        let mut input = Split::new(data, &[], true);
        let mut parser = Parser::default();
        let err = parser.feed(&mut input).unwrap_err();
        match err {
            FeedError::Incomplete(Some(n)) => assert_eq!(n.get(), 5),
            other => panic!("expected Incomplete(Some(5)), got {other:?}"),
        }
    }

    #[test]
    fn incomplete_head_reports_none() {
        // Partial status line.
        let data = b"RTSP/1.0 200";
        let mut input = Split::new(data, &[], true);
        let mut parser = Parser::default();
        let err = parser.feed(&mut input).unwrap_err();
        match err {
            FeedError::Incomplete(None) => {}
            other => panic!("expected Incomplete(None), got {other:?}"),
        }
    }

    fn messages<'i, I: Input<'i>>(parser: &mut Parser, input: &mut I) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            match parser.feed(input) {
                Ok(Some((Message::Data(d), body))) => {
                    seen.push(format!("data {} {}", d.channel_id, body.to_cow().len()));
                }
                Ok(Some((Message::Response(r), _))) => seen.push(format!(
                    "response {} {}",
                    r.headers.get("CSeq").unwrap(),
                    r.headers.get("Public").unwrap()
                )),
                Ok(Some((Message::Request(_), _))) => panic!("unexpected request"),
                Ok(None) | Err(FeedError::Incomplete(_)) => return seen,
                Err(e) => panic!("{e}"),
            }
        }
    }

    fn rtp_frame(len: u16, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![b'$', 0, (len >> 8) as u8, len as u8];
        frame.extend_from_slice(&[
            0x80, 0x60, 0x13, 0x93, 0x66, 0xa5, 0x55, 0x12, 0x44, 0xcc, 0x66, 0xfc,
        ]);
        frame.extend_from_slice(payload);
        frame.resize(4 + usize::from(len), 0x2c);
        frame
    }

    fn messages_at_every_split(data: &[u8], expected: &[&str]) {
        for split in 0..=data.len() {
            let (first, second) = data.split_at(split);

            let mut input = Split::new(first, second, false);
            assert_eq!(
                messages(&mut Parser::default(), &mut input),
                expected,
                "split {split}"
            );

            let mut parser = Parser::default();
            let mut p1 = Split::new(first, &[], true);
            let mut seen = messages(&mut parser, &mut p1);
            let consumed = first.len() - p1.len();
            let mut p2 = Split::new(&first[consumed..], second, false);
            seen.extend(messages(&mut parser, &mut p2));
            assert_eq!(seen, expected, "split {split}");
        }
    }

    #[test]
    fn frame_inside_a_header_line() {
        let mut frame = vec![
            b'$', 0, 0, 60, 0x80, 0x60, 0x40, 0x43, 0x60, 0xf5, 0x56, 0xa6,
        ];
        frame.extend_from_slice(b"\r\n");
        frame.resize(4 + 60, 0x2c);
        let mut data = b"RTSP/1.0 200 OK\r\nCSeq: 5\r\nDate: Tue Feb  1 01:47:58 2000 GMT\r\n\
                         Public: OPTIONS, DESCRIBE, S"
            .to_vec();
        data.extend_from_slice(&frame);
        data.extend_from_slice(b"ETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER\r\n\r\n");
        messages_at_every_split(
            &data,
            &[
                "data 0 60",
                "response 5 OPTIONS, DESCRIBE, SETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER",
            ],
        );
    }

    #[test]
    fn frames_inside_the_status_line() {
        let mut data = b"RTSP".to_vec();
        data.extend(rtp_frame(
            25,
            b"\x67\x4d\x40\x28\x95\xa0\x1e\x00\x89\xf9\x66\xc0\x40",
        ));
        data.extend(rtp_frame(16, b"\x68\xee\x3c\x80"));
        data.extend(rtp_frame(60, b"\x06\xf0\x2c\x0f\x45\xd4\x23\x0b"));
        data.extend_from_slice(b"/1.0 200 OK\r\nCSeq: 5\r\nPublic: OPTIONS, DESCRIBE\r\n\r\n");
        messages_at_every_split(
            &data,
            &[
                "data 0 25",
                "data 0 16",
                "data 0 60",
                "response 5 OPTIONS, DESCRIBE",
            ],
        );
    }

    #[test]
    fn frames_between_cr_and_lf() {
        let mut data = b"RTSP/1.0 200 OK\r".to_vec();
        data.extend(rtp_frame(60, b"\x06\xf0\x2c"));
        data.extend_from_slice(b"\nCSeq: 5\r");
        data.extend(rtp_frame(16, b"\x68\xee\x3c\x80"));
        data.extend_from_slice(b"\nPublic: OPTIONS, DESCRIBE\r\n\r");
        data.extend(rtp_frame(25, b"\x67\x4d\x40\x28"));
        data.extend_from_slice(b"\n");
        messages_at_every_split(
            &data,
            &[
                "data 0 60",
                "data 0 16",
                "data 0 25",
                "response 5 OPTIONS, DESCRIBE",
            ],
        );
    }

    #[test]
    fn dollar_in_a_header_is_text() {
        messages_at_every_split(
            b"RTSP/1.0 200 OK\r\nCSeq: 7\r\nPublic: A$BCD\r\n\r\n\
              RTSP/1.0 200 O$K\r\nCSeq: 8\r\nPublic: E$\tF\r\n\r\n\
              RTSP/1.0 200 OK\r\nCSeq: 9\r\nPublic: G$\r\n\r\n",
            &["response 7 A$BCD", "response 8 E$\tF", "response 9 G$"],
        );
    }

    #[test]
    fn a_blank_line_at_the_end_of_a_read_is_consumed_once() {
        let mut p = Parser::default();
        let mut buf: Vec<u8> = Vec::new();
        let mut delivered = 0;
        let mut f = vec![b'$', 0, 0, 40, 0x80, 0x60];
        f.resize(44, 0xab);
        for read in [&b"\r\n"[..], &f[..]] {
            buf.extend_from_slice(read);
            loop {
                let mut input = Contiguous::new(&buf, true);
                let before = input.len();
                match p.feed(&mut input) {
                    Ok(Some((Message::Data(_), _))) => {
                        assert_eq!(p.msg_start_pos(), 2);
                        delivered += 1;
                        let used = before - input.len();
                        buf.drain(..used);
                    }
                    Ok(Some(_)) => unreachable!(),
                    Ok(None) | Err(FeedError::Incomplete(_)) => {
                        let used = before - input.len();
                        buf.drain(..used);
                        break;
                    }
                    Err(FeedError::Invalid(e)) => panic!("invalid: {e}"),
                }
            }
        }
        assert_eq!(delivered, 1);
        assert!(buf.is_empty());
        assert_eq!(p.stream_pos(), 2 + 44);
        assert_eq!(p.msg_start_pos(), 2 + 44);
    }

    #[test]
    fn message_start_pos_tracks_origin_across_reads_and_blank_lines() {
        let mut parser = Parser::default();
        let mut buf = Vec::new();

        buf.extend_from_slice(b"$\x00\x00\x04asdf");
        let mut input = Contiguous::new(&buf, true);
        let before = input.len();
        assert!(matches!(
            parser.feed(&mut input).unwrap(),
            Some((Message::Data(_), _))
        ));
        assert_eq!(parser.msg_start_pos(), 0);
        buf.drain(..before - input.len());

        buf.extend_from_slice(b"\r\nRTSP/1.0 200 OK\r\nCSe");
        let mut input = Contiguous::new(&buf, true);
        let before = input.len();
        assert!(matches!(
            parser.feed(&mut input),
            Err(FeedError::Incomplete(None))
        ));
        assert_eq!(parser.msg_start_pos(), 10);
        buf.drain(..before - input.len());

        buf.extend_from_slice(b"q: 1\r\n\r\n");
        let mut input = Contiguous::new(&buf, true);
        assert!(matches!(
            parser.feed(&mut input).unwrap(),
            Some((Message::Response(_), _))
        ));
        assert_eq!(parser.msg_start_pos(), 10);
    }

    #[test]
    fn max_message_size_allows_embedded_rtp_frame_and_bounds_header_text() {
        let mut data = b"RTSP".to_vec();
        data.extend(rtp_frame(120, b"\x67\x4d"));
        data.extend_from_slice(b"/1.0 200 OK\r\nCSeq: 5\r\nPublic: OPTIONS\r\n\r\n");
        let mut parser = Parser::builder().max_message_size(64).build();
        let mut input = Split::new(&data, &[], false);
        assert_eq!(
            messages(&mut parser, &mut input),
            &["data 0 120", "response 5 OPTIONS"]
        );

        let mut oversized = b"RTSP/1.0 200 OK\r\nX: ".to_vec();
        oversized.extend_from_slice(&[b'a'; 64]);
        oversized.extend(rtp_frame(16, b"\x68"));
        oversized.extend_from_slice(b"\r\n\r\n");
        let mut parser = Parser::builder().max_message_size(64).build();
        let mut input = Split::new(&oversized, &[], false);
        assert!(matches!(
            parser.feed(&mut input),
            Err(FeedError::Invalid(_))
        ));
    }
}

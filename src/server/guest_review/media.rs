use super::super::{ServerState, authentication, sharing};
use super::{
    codecs,
    http::{control_failure, failure, handle_cookie, io_failure, session_id},
    ranges,
    registry::{RequestGuard, TransferGuard},
    verified::{VERIFY_TIMEOUT, Verified},
};
use crate::{
    access::ClientClassificationReason,
    shares::{ExportBinding, GrantSummary, Mode, SessionContext, Ticket},
    storage::{catalog::readers::LeaseSet, volumes::root::OwnedFile},
};
use rouille::{Request, Response, ResponseBody};
use std::{
    io::{self, Read, Seek, SeekFrom},
    net::IpAddr,
    time::Instant,
};

pub(super) fn response(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
    guard: RequestGuard,
    mode: Mode,
) -> Response {
    prepare(request, state, context, guard, mode).unwrap_or_else(|response| response)
}

struct Admission {
    bound: crate::shares::BoundSession,
    range: ranges::ByteRange,
    guard: TransferGuard,
    owner: Delivery,
}

fn prepare(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
    guard: RequestGuard,
    mode: Mode,
) -> Result<Response, Response> {
    let admission = admission(request, state, context, guard, mode)?;
    let binding = &admission.bound.grant.intent.binding;
    let (mut file, lease) = verified(state, binding, &admission.guard)?;
    file.seek(SeekFrom::Start(admission.range.start))
        .map_err(io_failure)?;
    let length =
        usize::try_from(admission.range.end - admission.range.start).map_err(|_| failure(503))?;
    let bytes = binding.bytes;
    let reader = MediaReader {
        file,
        _lease: lease,
        owner: admission.owner,
        grant: admission.bound.grant,
        guard: admission.guard,
        peer: request.remote_addr().ip(),
        remaining: length,
    };
    Ok(sized_response(reader, admission.range, bytes, mode))
}

fn admission(
    request: &Request,
    state: &ServerState,
    context: &SessionContext,
    guard: RequestGuard,
    mode: Mode,
) -> Result<Admission, Response> {
    let id = session_id(request, true)?.ok_or_else(|| failure(400))?;
    let handle = handle_cookie(request)?;
    let bound = sharing::with_guest_store(state, context, |store, clock| {
        store.inspect_session(handle, Some(id), context, clock)
    })
    .map_err(control_failure)?;
    let checked = sharing::validate_guest(state, &bound.grant).map_err(control_failure)?;
    let range = ranges::parse(
        authentication::header(request, "Range", 128)?,
        checked.binding.bytes,
    )
    .map_err(|_| {
        failure(416).with_additional_header(
            "Content-Range",
            format!("bytes */{}", checked.binding.bytes),
        )
    })?;
    let guard = guard.bind(&bound, context).map_err(io_failure)?;
    let ticket = sharing::with_guest_store(state, context, |store, clock| {
        store.begin_delivery(handle, context, &checked.binding, mode, clock)
    })
    .map_err(control_failure)?;
    let classification = state
        .network_access
        .classify(request.remote_addr().ip(), request.headers());
    let owner = Delivery {
        state: state.clone(),
        ticket: Some(ticket),
        committed: false,
        mode,
        context: context.clone(),
        grant_id: bound.grant.id,
        classification: classification.reason,
    };
    Ok(Admission {
        bound,
        range,
        guard,
        owner,
    })
}

fn verified(
    state: &ServerState,
    binding: &ExportBinding,
    guard: &TransferGuard,
) -> Result<(Verified<PinnedReader>, LeaseSet), Response> {
    let _permit = state.guest_review.reserve_hash().map_err(io_failure)?;
    let opened = sharing::open_guest(state, binding).map_err(control_failure)?;
    let deadline = Instant::now()
        .checked_add(VERIFY_TIMEOUT)
        .ok_or_else(|| failure(503))?;
    let check = || {
        guard.check()?;
        if Instant::now() >= deadline {
            return Err(io::Error::other("guest verification timed out"));
        }
        Ok(())
    };
    let mut file = Verified::new(
        PinnedReader(opened.file),
        binding.bytes,
        binding.sha256,
        &check,
    )
    .map_err(io_failure)?;
    codecs::inspect(&mut file, binding.bytes, check).map_err(io_failure)?;
    Ok((file, opened.lease))
}

fn sized_response(
    reader: MediaReader,
    range: ranges::ByteRange,
    bytes: u64,
    mode: Mode,
) -> Response {
    let length = reader.remaining;
    let mut response = Response {
        status_code: if range.partial { 206 } else { 200 },
        headers: Vec::new(),
        data: ResponseBody::from_reader_and_size(reader, length),
        upgrade: None,
    }
    .with_additional_header("Content-Type", "video/mp4")
    .with_additional_header("Accept-Ranges", "bytes");
    if range.partial {
        response = response.with_additional_header(
            "Content-Range",
            format!("bytes {}-{}/{bytes}", range.start, range.end - 1),
        );
    }
    response.with_additional_header(
        "Content-Disposition",
        match mode {
            Mode::Stream => "inline; filename=\"shared-evidence.mp4\"",
            Mode::Download => "attachment; filename=\"shared-evidence.mp4\"",
        },
    )
}
struct PinnedReader(OwnedFile);

impl Read for PinnedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.0.read(output)
    }
}

impl Seek for PinnedReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.0.revalidate().map_err(io::Error::other)?;
        let position = self.0.file_mut().seek(from)?;
        self.0.revalidate().map_err(io::Error::other)?;
        Ok(position)
    }
}

struct MediaReader {
    file: Verified<PinnedReader>,
    _lease: LeaseSet,
    owner: Delivery,
    grant: GrantSummary,
    guard: TransferGuard,
    peer: IpAddr,
    remaining: usize,
}

impl MediaReader {
    fn deliver(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.remaining == 0 {
            return Ok(0);
        }
        self.guard.check()?;
        let bytes = self.file.peek(output.len().min(self.remaining))?;
        let length = bytes.len();
        if length == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "guest artifact ended",
            ));
        }
        self.guard.pace(self.peer, length)?;
        sharing::validate_guest(&self.owner.state, &self.grant).map_err(|_| denied())?;
        self.owner.deliver(&self.grant.intent.binding)?;
        self.guard.check()?;
        output[..length].copy_from_slice(bytes);
        self.file.consume(length);
        self.remaining -= length;
        Ok(length)
    }
}

impl Read for MediaReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self.deliver(output) {
            Ok(length) => Ok(length),
            Err(error) => {
                self.guard.abort();
                Err(error)
            }
        }
    }
}

struct Delivery {
    state: ServerState,
    ticket: Option<Ticket>,
    committed: bool,
    mode: Mode,
    context: SessionContext,
    grant_id: uuid::Uuid,
    classification: ClientClassificationReason,
}

impl Delivery {
    fn deliver(&mut self, binding: &ExportBinding) -> io::Result<()> {
        let ticket = self.ticket.as_ref().ok_or_else(denied)?;
        sharing::with_guest_store(&self.state, &self.context, |store, clock| {
            if self.committed {
                store.authorize(ticket, &self.context, binding, clock)
            } else {
                store.commit_delivery(ticket, &self.context, binding, clock)
            }
        })
        .map_err(|_| denied())?;
        if !self.committed {
            self.committed = true;
            super::super::record_access_audit(
                &self.state,
                i64::try_from(super::super::unix_time_ms()).unwrap_or(i64::MAX),
                None,
                None,
                "guest_delivery",
                Some(&self.grant_id.to_string()),
                match self.mode {
                    Mode::Stream => "stream_started",
                    Mode::Download => "download_started",
                },
                self.classification,
            );
        }
        Ok(())
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take()
            && sharing::with_guest_store(&self.state, &self.context, |store, _| {
                store.finish(ticket);
                Ok(())
            })
            .is_err_and(|error| error._http_status == 503)
        {
            tracing::warn!("guest transfer cleanup was unavailable");
        }
    }
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "guest transfer is unavailable",
    )
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;

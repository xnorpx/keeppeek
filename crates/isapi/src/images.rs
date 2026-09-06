use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::error::Kind;
use crate::{Error, Event, Part, PartKind};

const PENDING_MAX: usize = 16;
const IMAGES_MAX: usize = 32;
const BYTES_MAX: usize = 8 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(5);
const RETIRED_TTL: Duration = Duration::from_secs(30);

/// An image matched to an explicit reference in one event.
#[derive(Clone)]
pub struct Image {
    id: String,
    body: Arc<[u8]>,
}

impl Image {
    /// Returns the event's reference identifier, not a filename to open.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Returns the original encoded JPEG bytes.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Shares the bounded image bytes without copying them.
    pub fn bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.body)
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Image")
            .field("byte_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// One notification and its explicitly associated images, possibly incomplete after expiry.
#[derive(Debug)]
pub struct Bundle {
    event: Event,
    images: Vec<Image>,
    received: Duration,
}

impl Bundle {
    /// Returns the notification that declared the image references.
    pub const fn event(&self) -> &Event {
        &self.event
    }
    /// Returns only images whose identifiers matched this notification.
    pub fn images(&self) -> &[Image] {
        &self.images
    }
    /// Returns the caller-supplied monotonic metadata receipt time.
    pub const fn received(&self) -> Duration {
        self.received
    }
    /// Reports whether every explicit image reference was satisfied.
    pub fn complete(&self) -> bool {
        self.images.len() == self.event.images().len()
    }
}

struct Pending {
    event: Event,
    received: Duration,
}
struct ReceivedImage {
    keys: Vec<String>,
    body: Arc<[u8]>,
    received: Duration,
}

/// Correlates images by Content-ID, filename or form name, never by proximity.
///
/// State is limited to 16 notifications, 32 images and 8 MiB of JPEG bytes.
/// Incomplete bundles expire after five seconds of caller-supplied time. Identifiers
/// remain reserved for thirty seconds after use. Use a new assembler per callback
/// request and reset it on an alert-stream reconnect.
pub struct Assembler {
    pending: Vec<Pending>,
    ready: Vec<Bundle>,
    images: Vec<ReceivedImage>,
    retired: BTreeMap<String, Duration>,
    now: Duration,
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Assembler {
    /// Returns the earliest pending image deadline in the caller's monotonic time domain.
    pub fn next_deadline(&self) -> Option<Duration> {
        self.pending
            .iter()
            .map(|pending| pending.received.saturating_add(TTL))
            .min()
    }
    /// Starts an empty correlation scope without accessing a clock.
    pub const fn new() -> Self {
        Self {
            pending: Vec::new(),
            ready: Vec::new(),
            images: Vec::new(),
            retired: BTreeMap::new(),
            now: Duration::ZERO,
        }
    }

    /// Consumes a part and returns complete or expired bundles.
    ///
    /// # Errors
    /// Rejects ambiguous identifiers, conflicting duplicate images, malformed JPEG framing,
    /// backwards time and resource exhaustion. Previously pending metadata remains recoverable.
    pub fn push(&mut self, part: Part, now: Duration) -> Result<Vec<Bundle>, Error> {
        self.advance(now)?;
        self.images
            .retain(|image| now.saturating_sub(image.received) < TTL);
        let expired = self.drain(now, false);
        self.ready.extend(expired);
        self.images
            .retain(|image| now.saturating_sub(image.received) < TTL);
        if let Some(event) = part.event()? {
            self.add_event(event, now)?;
        } else if part.kind() == PartKind::Jpeg {
            self.add_image(part, now)?;
        }
        let completed = self.drain(now, false);
        self.ready.extend(completed);
        Ok(std::mem::take(&mut self.ready))
    }

    /// Emits incomplete metadata at its deadline and discards expired orphan images.
    ///
    /// # Errors
    /// Rejects a caller clock that moved backwards.
    pub fn expire(&mut self, now: Duration) -> Result<Vec<Bundle>, Error> {
        self.advance(now)?;
        self.images
            .retain(|image| now.saturating_sub(image.received) < TTL);
        let bundles = self.drain(now, false);
        self.ready.extend(bundles);
        self.images
            .retain(|image| now.saturating_sub(image.received) < TTL);
        Ok(std::mem::take(&mut self.ready))
    }

    /// Emits all pending metadata and clears every association across a transport boundary.
    pub fn finish(&mut self) -> Vec<Bundle> {
        let bundles = self.drain(self.now, true);
        self.ready.extend(bundles);
        self.images.clear();
        self.retired.clear();
        std::mem::take(&mut self.ready)
    }

    fn advance(&mut self, now: Duration) -> Result<(), Error> {
        if now < self.now {
            return Err(Error::new(Kind::InvalidInput));
        }
        self.now = now;
        self.retired
            .retain(|_, used| now.saturating_sub(*used) < RETIRED_TTL);
        Ok(())
    }

    fn add_event(&mut self, event: Event, received: Duration) -> Result<(), Error> {
        if self.pending.len() + self.ready.len() >= PENDING_MAX {
            return Err(Error::new(Kind::Limit));
        }
        self.reserve_identifiers(event.images().len())?;
        for reference in event.images() {
            let key = identifier(reference.id())?;
            if self.retired.contains_key(&key)
                || self.pending.iter().any(|pending| {
                    pending
                        .event
                        .images()
                        .iter()
                        .any(|reference| identifier(reference.id()).is_ok_and(|other| other == key))
                })
            {
                return Err(Error::new(Kind::Protocol));
            }
        }
        for image in &self.images {
            if event
                .images()
                .iter()
                .chain(
                    self.pending
                        .iter()
                        .flat_map(|pending| pending.event.images()),
                )
                .filter(|reference| matches(image, reference.id()))
                .count()
                > 1
            {
                return Err(Error::new(Kind::Protocol));
            }
        }
        self.pending.push(Pending { event, received });
        Ok(())
    }

    fn add_image(&mut self, part: Part, received: Duration) -> Result<(), Error> {
        let mut keys = Vec::new();
        for value in [part.content_id(), part.filename()].into_iter().flatten() {
            let key = identifier(value)?;
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        if keys.is_empty()
            && let Some(name) = part.name()
        {
            keys.push(identifier(name)?);
        }
        if keys.is_empty() {
            return Ok(());
        }
        self.reserve_identifiers(keys.len())?;
        if !part.body().starts_with(&[0xff, 0xd8])
            || !part.body().ends_with(&[0xff, 0xd9])
            || part.body().len() < 4
        {
            return Err(Error::new(Kind::Protocol));
        }
        if self.images.iter().any(|image| {
            image.keys.iter().any(|key| keys.contains(key)) && image.body.as_ref() != part.body()
        }) {
            return Err(Error::new(Kind::Protocol));
        }
        if self
            .images
            .iter()
            .any(|image| image.keys.iter().any(|key| keys.contains(key)))
        {
            return Ok(());
        }
        if keys.iter().any(|key| self.retired.contains_key(key)) {
            return Err(Error::new(Kind::Protocol));
        }
        let ready_count = self
            .ready
            .iter()
            .map(|bundle| bundle.images.len())
            .sum::<usize>();
        let ready_bytes = self
            .ready
            .iter()
            .flat_map(|bundle| &bundle.images)
            .map(|image| image.body.len())
            .sum::<usize>();
        if self.images.len() + ready_count >= IMAGES_MAX
            || part.body().len()
                > BYTES_MAX.saturating_sub(
                    ready_bytes
                        + self
                            .images
                            .iter()
                            .map(|image| image.body.len())
                            .sum::<usize>(),
                )
        {
            return Err(Error::new(Kind::Limit));
        }
        let image = ReceivedImage {
            keys,
            body: part.into_body().into(),
            received,
        };
        let claims = self
            .pending
            .iter()
            .flat_map(|pending| pending.event.images())
            .filter(|reference| matches(&image, reference.id()))
            .count();
        if claims > 1 {
            return Err(Error::new(Kind::Protocol));
        }
        self.images.push(image);
        Ok(())
    }

    fn drain(&mut self, now: Duration, all: bool) -> Vec<Bundle> {
        let mut bundles = Vec::new();
        let mut index = 0;
        while index < self.pending.len() {
            let pending = &self.pending[index];
            if !all
                && self.pending[..index]
                    .iter()
                    .any(|earlier| same_event(&earlier.event, &pending.event))
            {
                index += 1;
                continue;
            }
            let complete = pending.event.images().iter().all(|reference| {
                self.images
                    .iter()
                    .any(|image| matches(image, reference.id()))
            });
            if !all && !complete && now.saturating_sub(pending.received) < TTL {
                index += 1;
                continue;
            }
            let pending = self.pending.remove(index);
            let mut images = Vec::new();
            for reference in pending.event.images() {
                if let Some(index) = self
                    .images
                    .iter()
                    .position(|image| matches(image, reference.id()))
                {
                    let image = self.images.remove(index);
                    for key in image.keys {
                        self.retired.insert(key, now);
                    }
                    images.push(Image {
                        id: reference.id().to_owned(),
                        body: image.body,
                    });
                }
                self.retired.insert(
                    identifier(reference.id()).expect("event references were validated"),
                    now,
                );
            }
            bundles.push(Bundle {
                event: pending.event,
                images,
                received: pending.received,
            });
        }
        bundles
    }

    fn reserve_identifiers(&self, additional: usize) -> Result<(), Error> {
        let reserved = self.retired.len()
            + self
                .pending
                .iter()
                .map(|pending| pending.event.images().len())
                .sum::<usize>()
            + self
                .images
                .iter()
                .map(|image| image.keys.len())
                .sum::<usize>();
        if additional > 512_usize.saturating_sub(reserved) {
            return Err(Error::new(Kind::Limit));
        }
        Ok(())
    }
}

impl fmt::Debug for Assembler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Assembler")
            .field("pending", &self.pending.len())
            .field("images", &self.images.len())
            .finish_non_exhaustive()
    }
}

fn identifier(value: &str) -> Result<String, Error> {
    let value = value.trim();
    let value = value.strip_prefix("cid:").unwrap_or(value);
    let value = value
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(value);
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(Error::new(Kind::Protocol));
    }
    Ok(value.to_owned())
}

fn matches(image: &ReceivedImage, reference: &str) -> bool {
    identifier(reference).is_ok_and(|key| image.keys.contains(&key))
}

fn same_event(first: &Event, second: &Event) -> bool {
    first.dynamic_channel_id().or_else(|| first.channel_id())
        == second.dynamic_channel_id().or_else(|| second.channel_id())
        && (first.active() == Some(false)
            || second.active() == Some(false)
            || (first.event_type().eq_ignore_ascii_case(second.event_type())
                && first
                    .id()
                    .zip(second.id())
                    .is_none_or(|(first, second)| first == second)))
}

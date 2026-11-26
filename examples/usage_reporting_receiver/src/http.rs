//! Bounded HTTP service for the publishable usage reporting receiver.
//!
//! Enforces strict body size caps, per-installation proof tokens, concurrency limits,
//! non-IP-persisting rate limits, and admin separation from public aggregate endpoints.

use crate::aggregate::compute_public_aggregates;
use crate::retention::run_expiry_pass;
use crate::schema::{
    MAX_PAYLOAD_BYTES, UsageReportV1, ValidationError, is_canonical_uuid_v4, parse_and_validate,
};
use crate::store::{ReceiverStore, StoreError, UpsertOutcome, hashes_match, sha256_bytes};
use rouille::{Request, Response};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_IN_FLIGHT_REQUESTS: usize = 64;
pub const MAX_RATE_BUCKETS: usize = 4_096;
pub const DEFAULT_PER_INSTALL_WINDOW_SECS: i64 = 60;
pub const DEFAULT_PER_INSTALL_LIMIT: u32 = 12;
pub const DEFAULT_GLOBAL_LIMIT: u32 = 2_000;
const MIN_TOKEN_LEN: usize = 32;
const MAX_TOKEN_LEN: usize = 128;

pub type ClockFn = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Clone)]
pub struct ReceiverConfig {
    pub admin_token_hash: Option<[u8; 32]>,
    pub max_in_flight: usize,
    pub per_install_window_secs: i64,
    pub per_install_limit: u32,
    pub global_limit: u32,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            admin_token_hash: None,
            max_in_flight: MAX_IN_FLIGHT_REQUESTS,
            per_install_window_secs: DEFAULT_PER_INSTALL_WINDOW_SECS,
            per_install_limit: DEFAULT_PER_INSTALL_LIMIT,
            global_limit: DEFAULT_GLOBAL_LIMIT,
        }
    }
}

impl ReceiverConfig {
    pub fn with_admin_token(mut self, token: &str) -> Self {
        self.admin_token_hash = Some(sha256_bytes(token.as_bytes()));
        self
    }
}

pub struct ReceiverService {
    store: Arc<Mutex<ReceiverStore>>,
    config: ReceiverConfig,
    clock: ClockFn,
    in_flight: Arc<AtomicUsize>,
    rate_limiter: Mutex<RateLimiter>,
}

impl ReceiverService {
    pub fn new(store: Arc<Mutex<ReceiverStore>>, config: ReceiverConfig) -> Self {
        Self::with_clock(store, config, Arc::new(system_unix_secs))
    }

    pub fn with_clock(
        store: Arc<Mutex<ReceiverStore>>,
        config: ReceiverConfig,
        clock: ClockFn,
    ) -> Self {
        let rate_limiter = RateLimiter::new(
            config.per_install_window_secs,
            config.per_install_limit,
            config.global_limit,
        );
        Self {
            store,
            config,
            clock,
            in_flight: Arc::new(AtomicUsize::new(0)),
            rate_limiter: Mutex::new(rate_limiter),
        }
    }

    pub fn handle(&self, request: &Request) -> Response {
        let Some(_guard) = InFlightGuard::try_acquire(&self.in_flight, self.config.max_in_flight)
        else {
            return error_response(503, "receiver_busy", "receiver concurrency limit reached");
        };
        let now_secs = (self.clock)();
        let path = request.url();
        let method = request.method();

        if path == "/v1/aggregates" {
            return if method == "GET" {
                self.handle_get_aggregates(now_secs)
            } else {
                error_response(405, "method_not_allowed", "use GET for public aggregates")
            };
        }
        if path == "/v1/admin/status" {
            return if method == "GET" {
                self.handle_admin_status(request, now_secs)
            } else {
                error_response(405, "method_not_allowed", "use GET for admin status")
            };
        }
        if path == "/v1/admin/expire" {
            return if method == "POST" {
                self.handle_admin_expire(request, now_secs)
            } else {
                error_response(405, "method_not_allowed", "use POST for admin expiry")
            };
        }
        if let Some(installation_id) = path.strip_prefix("/v1/installations/") {
            return match method {
                "PUT" => self.handle_put_installation(request, installation_id, now_secs),
                "DELETE" => self.handle_delete_installation(request, installation_id, now_secs),
                // Raw installation rows are never readable via HTTP.
                "GET" => error_response(
                    405,
                    "method_not_allowed",
                    "raw installation rows are not readable",
                ),
                _ => error_response(405, "method_not_allowed", "unsupported method"),
            };
        }
        error_response(404, "not_found", "endpoint not found")
    }

    fn handle_put_installation(
        &self,
        request: &Request,
        installation_id: &str,
        now_secs: i64,
    ) -> Response {
        if !is_canonical_uuid_v4(installation_id) {
            return error_response(400, "invalid_installation_id", "invalid UUID v4");
        }
        if !self.check_rate_limit(installation_id, now_secs) {
            return error_response(429, "rate_limited", "rate limit exceeded")
                .with_additional_header("Retry-After", "60");
        }
        let Some(token) = extract_bearer_token(request) else {
            return error_response(401, "unauthorized", "missing or malformed bearer token");
        };
        let (report, canonical_json) = match parse_installation_put_report(request, installation_id)
        {
            Ok(parsed) => parsed,
            Err(response) => return response,
        };

        let mut store = match self.store.lock() {
            Ok(guard) => guard,
            Err(_) => return error_response(503, "storage_unavailable", "store unavailable"),
        };
        match store.upsert_report(&report, &canonical_json, token, now_secs) {
            Ok(outcome) => {
                let status = match outcome {
                    UpsertOutcome::Replaced => "replaced",
                    UpsertOutcome::Idempotent => "idempotent",
                };
                no_store_json(
                    200,
                    &UpsertResponse {
                        status,
                        schema_version: report.schema_version,
                    },
                )
            }
            Err(StoreError::Unauthorized) => {
                error_response(403, "forbidden", "installation proof token mismatch")
            }
            Err(StoreError::StaleSequence) => {
                error_response(409, "stale_sequence", "report sequence is stale")
            }
            Err(StoreError::Deleted) => error_response(
                410,
                "installation_deleted",
                "installation identity was deleted and cannot be resurrected",
            ),
            Err(StoreError::Storage(_)) => {
                error_response(503, "storage_unavailable", "storage write failed")
            }
        }
    }

    fn handle_delete_installation(
        &self,
        request: &Request,
        installation_id: &str,
        now_secs: i64,
    ) -> Response {
        if !is_canonical_uuid_v4(installation_id) {
            return error_response(400, "invalid_installation_id", "invalid UUID v4");
        }
        if !self.check_rate_limit(installation_id, now_secs) {
            return error_response(429, "rate_limited", "rate limit exceeded")
                .with_additional_header("Retry-After", "60");
        }
        let Some(token) = extract_bearer_token(request) else {
            return error_response(401, "unauthorized", "missing or malformed bearer token");
        };

        let mut store = match self.store.lock() {
            Ok(guard) => guard,
            Err(_) => return error_response(503, "storage_unavailable", "store unavailable"),
        };
        match store.delete_installation(installation_id, token, now_secs) {
            Ok(_) => Response::empty_204()
                .with_additional_header("Cache-Control", "no-store")
                .with_additional_header("X-Content-Type-Options", "nosniff"),
            Err(StoreError::Unauthorized) => {
                error_response(403, "forbidden", "installation proof token mismatch")
            }
            Err(_) => error_response(503, "storage_unavailable", "storage delete failed"),
        }
    }

    fn handle_get_aggregates(&self, now_secs: i64) -> Response {
        let mut store = match self.store.lock() {
            Ok(guard) => guard,
            Err(_) => return error_response(503, "storage_unavailable", "store unavailable"),
        };
        // Purge expired rows before computing public output so stale rows never contribute.
        let _ = run_expiry_pass(&mut store, now_secs);
        let reports = match store.active_reports(now_secs) {
            Ok(rows) => rows,
            Err(_) => return error_response(503, "storage_unavailable", "aggregate query failed"),
        };
        let dataset = compute_public_aggregates(&reports, now_secs);
        Response::json(&dataset)
            .with_additional_header("Cache-Control", "public, max-age=300")
            .with_additional_header("X-Content-Type-Options", "nosniff")
    }

    fn handle_admin_status(&self, request: &Request, now_secs: i64) -> Response {
        if let Err(response) = self.authorize_admin(request) {
            return response;
        }
        let store = match self.store.lock() {
            Ok(guard) => guard,
            Err(_) => return error_response(503, "storage_unavailable", "store unavailable"),
        };
        store.admin_stats(now_secs).map_or_else(
            |_| error_response(503, "storage_unavailable", "status query failed"),
            |stats| {
                no_store_json(
                    200,
                    &AdminStatusResponse {
                        active_installations: stats.active_installations,
                        active_tombstones: stats.active_tombstones,
                        expired_installations: stats.expired_installations,
                    },
                )
            },
        )
    }

    fn handle_admin_expire(&self, request: &Request, now_secs: i64) -> Response {
        if let Err(response) = self.authorize_admin(request) {
            return response;
        }
        let mut store = match self.store.lock() {
            Ok(guard) => guard,
            Err(_) => return error_response(503, "storage_unavailable", "store unavailable"),
        };
        run_expiry_pass(&mut store, now_secs).map_or_else(
            |_| error_response(503, "storage_unavailable", "expiry pass failed"),
            |report| {
                no_store_json(
                    200,
                    &AdminExpireResponse {
                        expired_installations: report.expired_installations,
                        expired_tombstones: report.expired_tombstones,
                    },
                )
            },
        )
    }

    fn authorize_admin(&self, request: &Request) -> Result<(), Response> {
        let Some(expected_hash) = self.config.admin_token_hash else {
            return Err(error_response(
                403,
                "admin_disabled",
                "admin endpoints are disabled",
            ));
        };
        let Some(token) = extract_bearer_token(request) else {
            return Err(error_response(
                401,
                "unauthorized",
                "missing admin bearer token",
            ));
        };
        let actual = sha256_bytes(token.as_bytes());
        if !hashes_match(&actual, &expected_hash) {
            return Err(error_response(
                403,
                "forbidden",
                "invalid admin bearer token",
            ));
        }
        Ok(())
    }

    fn check_rate_limit(&self, installation_id: &str, now_secs: i64) -> bool {
        let key = sha256_bytes(installation_id.as_bytes());
        let Ok(mut limiter) = self.rate_limiter.lock() else {
            return false;
        };
        limiter.allow(key, now_secs)
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: &'a str,
}

#[derive(Serialize)]
struct UpsertResponse<'a> {
    status: &'a str,
    schema_version: u32,
}

#[derive(Serialize)]
struct AdminStatusResponse {
    active_installations: u64,
    active_tombstones: u64,
    expired_installations: u64,
}

#[derive(Serialize)]
struct AdminExpireResponse {
    expired_installations: u64,
    expired_tombstones: u64,
}

fn no_store_json<T: Serialize>(status: u16, body: &T) -> Response {
    Response::json(body)
        .with_status_code(status)
        .with_additional_header("Cache-Control", "no-store")
        .with_additional_header("X-Content-Type-Options", "nosniff")
}

fn error_response(status: u16, code: &str, message: &str) -> Response {
    no_store_json(
        status,
        &ErrorBody {
            error: code,
            message,
        },
    )
}

fn has_json_content_type(request: &Request) -> bool {
    request
        .header("Content-Type")
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
}

fn extract_bearer_token(request: &Request) -> Option<&str> {
    let raw = request.header("Authorization")?;
    let token = raw.strip_prefix("Bearer ")?.trim();
    if !(MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&token.len()) {
        return None;
    }
    if !token
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return None;
    }
    Some(token)
}

enum BodyReadError {
    TooLarge,
    ReadFailed,
}

fn read_bounded_body(request: &Request, max_bytes: usize) -> Result<Vec<u8>, BodyReadError> {
    let Some(body) = request.data() else {
        return Err(BodyReadError::ReadFailed);
    };
    let mut buffer = Vec::new();
    let limit = u64::try_from(max_bytes.saturating_add(1)).unwrap_or(u64::MAX);
    body.take(limit)
        .read_to_end(&mut buffer)
        .map_err(|_| BodyReadError::ReadFailed)?;
    if buffer.len() > max_bytes {
        return Err(BodyReadError::TooLarge);
    }
    Ok(buffer)
}

struct InFlightGuard {
    counter: Arc<AtomicUsize>,
}

impl InFlightGuard {
    fn try_acquire(counter: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= max {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Self {
                        counter: Arc::clone(counter),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

struct RateLimiter {
    window_secs: i64,
    per_install_limit: u32,
    global_limit: u32,
    window_start: i64,
    global_count: u32,
    buckets: HashMap<[u8; 32], u32>,
}

impl RateLimiter {
    fn new(window_secs: i64, per_install_limit: u32, global_limit: u32) -> Self {
        Self {
            window_secs: window_secs.max(1),
            per_install_limit: per_install_limit.max(1),
            global_limit: global_limit.max(1),
            window_start: 0,
            global_count: 0,
            buckets: HashMap::new(),
        }
    }

    fn allow(&mut self, key: [u8; 32], now_secs: i64) -> bool {
        if now_secs < self.window_start || now_secs - self.window_start >= self.window_secs {
            self.window_start = now_secs;
            self.global_count = 0;
            self.buckets.clear();
        }
        if self.global_count >= self.global_limit {
            return false;
        }
        if !self.buckets.contains_key(&key) && self.buckets.len() >= MAX_RATE_BUCKETS {
            return false;
        }
        let count = self.buckets.entry(key).or_insert(0);
        if *count >= self.per_install_limit {
            return false;
        }
        *count += 1;
        self.global_count += 1;
        true
    }
}

fn parse_installation_put_report(
    request: &Request,
    installation_id: &str,
) -> Result<(UsageReportV1, String), Response> {
    if !has_json_content_type(request) {
        return Err(error_response(
            415,
            "unsupported_media_type",
            "request must use application/json",
        ));
    }
    let body = match read_bounded_body(request, MAX_PAYLOAD_BYTES) {
        Ok(bytes) => bytes,
        Err(BodyReadError::TooLarge) => {
            return Err(error_response(
                413,
                "payload_too_large",
                "payload exceeds 65536 uncompressed bytes",
            ));
        }
        Err(BodyReadError::ReadFailed) => {
            return Err(error_response(
                400,
                "invalid_body",
                "failed to read request body",
            ));
        }
    };
    let (report, canonical_json) = match parse_and_validate(&body) {
        Ok(parsed) => parsed,
        Err(ValidationError::PayloadTooLarge) => {
            return Err(error_response(
                413,
                "payload_too_large",
                "payload exceeds 65536 uncompressed bytes",
            ));
        }
        Err(_) => {
            return Err(error_response(
                400,
                "schema_validation_failed",
                "payload failed closed v1 schema validation",
            ));
        }
    };
    if report.installation_id != installation_id {
        return Err(error_response(
            400,
            "installation_id_mismatch",
            "path installation_id must match payload installation_id",
        ));
    }
    Ok((report, canonical_json))
}

fn system_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

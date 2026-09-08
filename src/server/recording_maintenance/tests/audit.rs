use super::{ApiPrincipal, Fixture, IpAddr, Ipv4Addr, SessionId};
use crate::storage::catalog::maintenance::jobs::{Action, Reason};
use std::sync::{Arc, Mutex};
use tracing_subscriber::prelude::*;

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<serde_json::Value>>>);

impl<Subscriber: tracing::Subscriber> tracing_subscriber::Layer<Subscriber> for Capture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, Subscriber>,
    ) {
        let mut fields = Fields(serde_json::Map::new());
        event.record(&mut fields);
        self.0
            .lock()
            .unwrap()
            .push(serde_json::Value::Object(fields.0));
    }
}

struct Fields(serde_json::Map<String, serde_json::Value>);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_owned(), format!("{value:?}").into());
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.insert(field.name().to_owned(), value.into());
    }
}

#[test]
fn audit_emits_exact_scope_reason_counts_and_redacts_private_details() {
    let fixture = Fixture::new();
    let preview = fixture.preview();
    let principal = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let mut job = fixture
        .catalog
        .as_ref()
        .unwrap()
        .handle()
        .recording_deletion_intent(
            &principal.id(),
            Action::Read {
                id: preview.job_id.clone(),
            },
        )
        .unwrap();
    job.reason = Reason::Privacy;
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        super::super::audit::record(
            &fixture.handler,
            SessionId::from_u64(0),
            &principal,
            &job,
            &Err(anyhow::anyhow!("private-failure-detail")),
        );
    });
    let events = capture.0.lock().unwrap().clone();
    let output = serde_json::to_string(&events).unwrap();
    assert!(!output.contains("private-failure-detail"));
    assert!(!output.contains(fixture.root.to_str().unwrap()));
    assert!(!output.contains(preview.confirmation_nonce.as_deref().unwrap()));
    let fields = events
        .iter()
        .find(|event| event["event"] == "recording_maintenance")
        .expect("a structured recording-maintenance audit event is required");
    assert_eq!(fields["principal_id"], principal.id());
    assert_eq!(fields["job_id"], job.id);
    assert_eq!(fields["source_id"], "front");
    assert_eq!(fields["stream_id"], "sub");
    assert_eq!(fields["reason"], "privacy");
    assert_eq!(fields["preview_revision"], preview.revision);
    assert_eq!(fields["object_count"], 1);
    assert_eq!(fields["bytes"], 64);
    assert_eq!(fields["start_ms"], 1_000);
    assert_eq!(fields["end_ms"], 2_000);
    assert_eq!(fields["hold_override"], false);
    assert_eq!(fields["result"], "failed");
    let stored = fixture
        .handler
        .state
        .access_manager
        .list_audit(1)
        .pop()
        .unwrap();
    assert_eq!(stored.target_id.as_deref(), Some(job.id.as_str()));
    assert_eq!(stored.result, "failed");
}

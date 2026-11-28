use std::collections::BTreeMap;

use serde_json::Value;
use tracing::{Event, Subscriber, field::Visit, span};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

use super::{LogHub, LogLevel, RedactingDestination, RedactingMakeWriter, source_timing_event};

const MAX_SPAN_FIELDS: usize = 32;
const MAX_EVENT_FIELDS: usize = 64;

const _: () = {
    assert!(MAX_SPAN_FIELDS > 0);
    assert!(MAX_SPAN_FIELDS <= MAX_EVENT_FIELDS);
};

#[derive(Clone)]
pub struct LogCaptureLayer {
    hub: LogHub,
    timing_output: Option<RedactingDestination>,
}

#[derive(Default)]
struct SpanFields {
    fields: BTreeMap<String, Value>,
}

impl LogCaptureLayer {
    pub const fn new(hub: LogHub) -> Self {
        Self {
            hub,
            timing_output: None,
        }
    }

    pub(super) fn with_formatted_timing(writer: RedactingMakeWriter) -> Self {
        Self {
            hub: writer.hub,
            timing_output: Some(writer.destination),
        }
    }
}

impl<S> Layer<S> for LogCaptureLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, context: Context<'_, S>) {
        let mut visitor = FieldVisitor::for_span(BTreeMap::new());
        attrs.record(&mut visitor);
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(SpanFields {
                fields: visitor.fields,
            });
        }
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, context: Context<'_, S>) {
        // Record values before locking span extensions so Debug impls that emit events cannot deadlock.
        let mut visitor = FieldVisitor::for_span(BTreeMap::new());
        values.record(&mut visitor);
        let Some(span) = context.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        if let Some(span_fields) = extensions.get_mut::<SpanFields>() {
            for (key, value) in visitor.fields {
                if span_fields.fields.contains_key(&key)
                    || span_fields.fields.len() < MAX_SPAN_FIELDS
                {
                    span_fields.fields.insert(key, value);
                }
            }
        } else {
            extensions.insert(SpanFields {
                fields: visitor.fields,
            });
        }
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        let metadata = event.metadata();
        let mut merged = BTreeMap::new();
        if let Some(scope) = context.event_scope(event) {
            for span in scope.from_root() {
                let extensions = span.extensions();
                if let Some(span_fields) = extensions.get::<SpanFields>() {
                    for (key, value) in &span_fields.fields {
                        if merged.contains_key(key) || merged.len() < MAX_EVENT_FIELDS {
                            merged.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
        }
        let mut visitor = FieldVisitor::for_event(merged);
        event.record(&mut visitor);
        let message = visitor
            .message
            .unwrap_or_else(|| metadata.name().to_owned());
        let (target, file, line) = resolve_event_origin(metadata, &mut visitor.fields);
        let entry = self.hub.record(
            LogLevel::from(metadata.level()),
            &target,
            message,
            visitor.fields,
            file.as_deref(),
            line,
        );
        // Field-level redaction preserves measured numbers even when the same digits
        // occur in an operator secret. Serialize the already sanitized captured record.
        if source_timing_event(metadata)
            && let Some(output) = &self.timing_output
            && let Ok(mut line) = serde_json::to_vec(&entry)
        {
            line.push(b'\n');
            output.write(&line);
        }
    }
}

fn resolve_event_origin(
    metadata: &tracing::Metadata<'_>,
    fields: &mut BTreeMap<String, Value>,
) -> (String, Option<String>, Option<u32>) {
    let mut target = metadata.target().to_owned();
    let mut file = metadata.file().map(ToOwned::to_owned);
    let mut line = metadata.line();
    if target == "log" {
        if let Some(Value::String(log_target)) = fields.remove("log.target") {
            target = log_target;
        }
        if let Some(Value::String(log_file)) = fields.remove("log.file") {
            file = Some(log_file);
        }
        if let Some(Value::Number(log_line)) = fields.remove("log.line")
            && let Some(line_num) = log_line.as_u64().and_then(|val| u32::try_from(val).ok())
        {
            line = Some(line_num);
        }
        fields.remove("log.module_path");
    }
    (target, file, line)
}

struct FieldVisitor {
    capture_message: bool,
    message: Option<String>,
    fields: BTreeMap<String, Value>,
    max_fields: usize,
}

impl FieldVisitor {
    const fn for_span(fields: BTreeMap<String, Value>) -> Self {
        Self {
            capture_message: false,
            message: None,
            fields,
            max_fields: MAX_SPAN_FIELDS,
        }
    }

    const fn for_event(fields: BTreeMap<String, Value>) -> Self {
        Self {
            capture_message: true,
            message: None,
            fields,
            max_fields: MAX_EVENT_FIELDS,
        }
    }

    fn record_value(&mut self, field: &tracing::field::Field, value: Value) {
        if self.capture_message && field.name() == "message" {
            self.message = value
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| Some(value.to_string()));
        } else if self.fields.contains_key(field.name()) || self.fields.len() < self.max_fields {
            self.fields.insert(field.name().to_owned(), value);
        }
    }
}

impl Visit for FieldVisitor {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.record_value(field, Value::Bool(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.record_value(field, Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.record_value(field, Value::Number(value.into()));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.record_value(
            field,
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number),
        );
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record_value(field, Value::String(value.to_owned()));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.record_value(field, Value::String(format!("{value:?}")));
    }
}

#[cfg(test)]
mod tests {
    use tracing::dispatcher::Dispatch;
    use tracing_subscriber::{Registry, prelude::*};

    use super::*;

    #[test]
    fn captures_typed_event_fields() {
        let hub = LogHub::default();
        let subscriber = Registry::default().with(LogCaptureLayer::new(hub.clone()));
        let dispatch = Dispatch::new(subscriber);

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::warn!(
                target: "keeppeek::camera",
                camera_id = "front",
                retries = 3_u64,
                connected = false,
                "camera reconnecting"
            );
        });

        let snapshot = hub.snapshot(None, 10);
        let entry = &snapshot.entries[0];
        assert_eq!(entry.level, LogLevel::Warn);
        assert_eq!(entry.target, "keeppeek::camera");
        assert_eq!(entry.message, "camera reconnecting");
        assert_eq!(entry.fields["camera_id"], "front");
        assert_eq!(entry.fields["retries"], 3);
        assert_eq!(entry.fields["connected"], false);
    }

    #[test]
    fn propagates_nested_span_fields_with_event_precedence() {
        let hub = LogHub::default();
        let subscriber = Registry::default().with(LogCaptureLayer::new(hub.clone()));
        let dispatch = Dispatch::new(subscriber);

        tracing::dispatcher::with_default(&dispatch, || {
            let outer = tracing::info_span!(
                "camera_session",
                camera_id = "cam-1",
                stream = "sub",
                dynamic_tag = tracing::field::Empty
            );
            let _outer_guard = outer.enter();
            outer.record("dynamic_tag", "recorded-later");
            let inner = tracing::info_span!("track", stream = "main", codec = "h264");
            let _inner_guard = inner.enter();
            tracing::info!(
                target: "keeppeek::rtsp",
                codec = "h265",
                "nested span event"
            );
        });

        let snapshot = hub.snapshot(None, 10);
        let entry = &snapshot.entries[0];
        assert_eq!(entry.fields["camera_id"], "cam-1");
        assert_eq!(entry.fields["stream"], "main");
        assert_eq!(entry.fields["dynamic_tag"], "recorded-later");
        assert_eq!(entry.fields["codec"], "h265");
    }

    #[test]
    fn bounds_span_fields_to_max_limit_and_avoids_on_record_reentrancy_deadlock() {
        struct EmittingDebug;
        impl std::fmt::Debug for EmittingDebug {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                tracing::info!(target: "keeppeek::test", "nested event inside Debug");
                f.write_str("emitted")
            }
        }

        let hub = LogHub::default();
        let dispatch = Dispatch::new(Registry::default().with(LogCaptureLayer::new(hub.clone())));
        tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!(
                "many_fields",
                f01 = 1,
                f02 = 2,
                f03 = 3,
                f04 = 4,
                f05 = 5,
                f06 = 6,
                f07 = 7,
                f08 = 8,
                f09 = 9,
                f10 = 10,
                f11 = 11,
                f12 = 12,
                f13 = 13,
                f14 = 14,
                f15 = 15,
                f16 = 16,
                f17 = 17,
                f18 = 18,
                f19 = 19,
                f20 = 20,
                f21 = 21,
                f22 = 22,
                f23 = 23,
                f24 = 24,
                f25 = 25,
                f26 = 26,
                f27 = 27,
                f28 = 28,
                f29 = 29,
                f30 = 30,
                f31 = 31,
                f32 = 32,
                extra = tracing::field::Empty
            );
            let _guard = span.enter();
            span.record("extra", tracing::field::debug(&EmittingDebug));
            let child = tracing::info_span!("extra", f33 = 33, f34 = 34);
            let _child_guard = child.enter();
            tracing::info!(target: "keeppeek::test", "bounded");
        });
        let entries = hub.snapshot(None, 10).entries;
        assert!(
            entries
                .iter()
                .any(|e| e.message == "nested event inside Debug")
        );
        let bounded = entries.iter().find(|e| e.message == "bounded").unwrap();
        assert_eq!(bounded.fields["f01"], 1);
        assert_eq!(bounded.fields["f32"], 32);
        assert_eq!(bounded.fields["f33"], 33);
        assert!(!bounded.fields.contains_key("extra"));
    }
}

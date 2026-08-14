use std::collections::BTreeMap;

use serde_json::Value;
use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{Layer, layer::Context};

use super::{LogHub, LogLevel};

#[derive(Clone)]
pub struct LogCaptureLayer {
    hub: LogHub,
}

impl LogCaptureLayer {
    pub const fn new(hub: LogHub) -> Self {
        Self { hub }
    }
}

impl<S> Layer<S> for LogCaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        let message = visitor
            .message
            .unwrap_or_else(|| metadata.name().to_owned());
        self.hub.record(
            LogLevel::from(metadata.level()),
            metadata.target(),
            message,
            visitor.fields,
            metadata.file(),
            metadata.line(),
        );
    }
}

#[derive(Default)]
struct EventVisitor {
    message: Option<String>,
    fields: BTreeMap<String, Value>,
}

impl EventVisitor {
    fn record_value(&mut self, field: &tracing::field::Field, value: Value) {
        if field.name() == "message" {
            self.message = value
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| Some(value.to_string()));
        } else {
            self.fields.insert(field.name().to_owned(), value);
        }
    }
}

impl Visit for EventVisitor {
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
}

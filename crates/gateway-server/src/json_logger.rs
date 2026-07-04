use serde::Serialize;
use std::collections::BTreeMap;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// A tracing Layer that emits structured JSON to stdout.
/// Designed for log aggregation systems (Loki, ELK, Datadog).
pub struct JsonLoggerLayer;

impl<S> Layer<S> for JsonLoggerLayer
where
    S: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = *metadata.level();

        // Build the JSON log entry.
        let mut entry = JsonLogEntry {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            level: level_to_str(level).to_string(),
            target: metadata.target().to_string(),
            fields: BTreeMap::new(),
        };

        // Extract fields from the event.
        let mut visitor = FieldVisitor {
            fields: &mut entry.fields,
        };
        event.record(&mut visitor);

        // Output as single-line JSON.
        if let Ok(json) = serde_json::to_string(&entry) {
            eprintln!("{}", json);
        }
    }
}

#[derive(Serialize)]
struct JsonLogEntry {
    timestamp: String,
    level: String,
    target: String,
    #[serde(flatten)]
    fields: BTreeMap<String, serde_json::Value>,
}

fn level_to_str(level: Level) -> &'static str {
    if level == Level::ERROR {
        "ERROR"
    } else if level == Level::WARN {
        "WARN"
    } else if level == Level::INFO {
        "INFO"
    } else if level == Level::DEBUG {
        "DEBUG"
    } else {
        "TRACE"
    }
}

/// Visitor that collects event fields into a BTreeMap.
struct FieldVisitor<'a> {
    fields: &'a mut BTreeMap<String, serde_json::Value>,
}

impl<'a> tracing::field::Visit for FieldVisitor<'a> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let key = field.name().to_string();
        // Skip the "message" field's debug wrapper quotes.
        let val = format!("{:?}", value);
        let val = val.trim_matches('"');
        self.fields
            .insert(key, serde_json::Value::String(val.to_string()));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::from(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::from(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::from(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if let Some(v) = serde_json::Number::from_f64(value) {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::Number(v));
        }
    }
}

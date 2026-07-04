use once_cell::sync::Lazy;
use serde::Serialize;
use std::sync::Mutex;
use tracing::Level;

/// A single log entry captured for the admin UI.
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

const MAX_LOG_ENTRIES: usize = 500;

/// Global in-memory ring buffer of recent log entries.
pub static LOG_BUFFER: Lazy<LogBuffer> = Lazy::new(LogBuffer::new);

pub struct LogBuffer {
    entries: Mutex<Vec<LogEntry>>,
}

impl LogBuffer {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::with_capacity(MAX_LOG_ENTRIES)),
        }
    }

    pub fn push(&self, level: Level, message: &str) {
        let entry = LogEntry {
            timestamp: chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
            level: match level {
                Level::ERROR => "ERROR",
                Level::WARN => "WARN",
                Level::INFO => "INFO",
                Level::DEBUG => "DEBUG",
                Level::TRACE => "TRACE",
            }
            .into(),
            message: message.into(),
        };

        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= MAX_LOG_ENTRIES {
                entries.remove(0);
            }
            entries.push(entry);
        }
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

/// A tracing Layer that writes to the in-memory log buffer.
pub struct LogBufferLayer;

impl<S> tracing_subscriber::Layer<S> for LogBufferLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        let mut visitor = StringVisitor::new();
        event.record(&mut visitor);
        LOG_BUFFER.push(level, &visitor.result);
    }
}

struct StringVisitor {
    result: String,
}

impl StringVisitor {
    fn new() -> Self {
        Self {
            result: String::new(),
        }
    }
}

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !self.result.is_empty() {
            self.result.push(' ');
        }
        if field.name() != "message" {
            self.result
                .push_str(&format!("{}={:?}", field.name(), value));
        } else {
            self.result.push_str(&format!("{:?}", value));
        }
    }
}

/// In-memory log buffer for the TUI logs panel.
///
/// A custom tracing Layer feeds WARN+ messages here instead of printing to
/// stderr, preventing log lines from corrupting the alternate-screen TUI.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::Level;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Maximum number of log entries kept in memory.
const MAX_LOG_ENTRIES: usize = 200;

/// One captured log entry.
#[derive(Clone)]
pub struct LogEntry {
    pub level: String,
    pub message: String,
    pub timestamp: String,
}

/// Shared ring-buffer of log entries. Clone the Arc to share it between the
/// tracing layer and the TUI.
pub type LogBuffer = Arc<Mutex<VecDeque<LogEntry>>>;

/// Create a new empty log buffer.
pub fn new_log_buffer() -> LogBuffer {
    Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES)))
}

// -- Custom tracing Layer -----------------------------------------------------

/// Tracing Layer that captures WARN+ events into a LogBuffer.
/// Replaces the default FmtSubscriber so nothing is printed to the terminal
/// while the TUI is active.
pub struct TuiLogLayer {
    buffer: LogBuffer,
}

impl TuiLogLayer {
    pub fn new(buffer: LogBuffer) -> Self {
        Self { buffer }
    }
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // Strip surrounding quotes added by the debug formatter
            let raw = format!("{:?}", value);
            self.0 = raw.trim_matches('"').to_string();
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for TuiLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        // Only capture WARN and ERROR -- INFO/DEBUG are too noisy for the panel
        if level > Level::WARN {
            return;
        }

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        if visitor.0.is_empty() {
            return;
        }

        let now = chrono::Utc::now().format("%H:%M:%S").to_string();
        let entry = LogEntry {
            level: level.to_string(),
            message: visitor.0,
            timestamp: now,
        };

        if let Ok(mut buf) = self.buffer.try_lock() {
            if buf.len() >= MAX_LOG_ENTRIES {
                buf.pop_front();
            }
            buf.push_back(entry);
        }
        // If the lock is held (rare), skip silently -- better to drop a message
        // than to block the tracing call site.
    }
}

//! The bounded telemetry channel. A single MPSC queue decouples request handling
//! (many producers, `try_send` only — never blocks) from the single background
//! writer (one consumer). When the queue is full, events are DROPPED, never
//! buffered unbounded and never awaited on the request path.

use tokio::sync::mpsc;

use crate::telemetry::event::TelemetryEvent;

pub type Sender = mpsc::Sender<TelemetryEvent>;
pub type Receiver = mpsc::Receiver<TelemetryEvent>;

/// Create the bounded queue. `capacity` is clamped to at least 1 so a
/// misconfigured `TELEMETRY_QUEUE_CAPACITY=0` can't panic `mpsc::channel`.
pub fn channel(capacity: usize) -> (Sender, Receiver) {
    mpsc::channel(capacity.max(1))
}

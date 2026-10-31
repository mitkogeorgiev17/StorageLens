//! Backend → UI events (PLAN §14).
//!
//! | Event | When |
//! |---|---|
//! | `scan://progress` | at most every [`PROGRESS_INTERVAL`] while a scan runs |
//! | `scan://done` | once, when a scan ends (complete, partial, or cancelled) |
//! | `scan://error` | once, when a scan fails outright |
//! | `dupes://progress` | while hashing duplicates (M6) |
//!
//! Scanners already limit progress callbacks to ~10 per second; [`Throttle`]
//! enforces the same ceiling here, so a future scanner that reports more often
//! cannot flood the webview.
//!
//! Emission goes through [`Emit`], so the scan logic is tested without a
//! running Tauri app.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use storagelens_core::ScanStatus;

pub const SCAN_PROGRESS: &str = "scan://progress";
pub const SCAN_DONE: &str = "scan://done";
pub const SCAN_ERROR: &str = "scan://error";
/// While `find_duplicates` hashes: [`DupesProgressEvent`].
pub const DUPES_PROGRESS: &str = "dupes://progress";
/// Before each item of an executing deletion: [`CleanupProgressDto`](crate::dto::CleanupProgressDto).
pub const CLEANUP_PROGRESS: &str = "cleanup://progress";

/// Minimum spacing between two progress events of one stream.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgressEvent {
    pub scan_id: i64,
    pub files: u64,
    pub dirs: u64,
    /// Allocated bytes seen so far.
    pub bytes_alloc: u64,
    pub elapsed_ms: u64,
    pub current_path: Option<String>,
    /// Files per second since the scan started.
    pub rate: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanDoneEvent {
    pub scan_id: i64,
    pub status: ScanStatus,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanErrorEvent {
    pub scan_id: i64,
    pub message: String,
}

/// Payload of [`DUPES_PROGRESS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupesProgressEvent {
    pub scan_id: i64,
    /// Size groups finished, of `groups_total`.
    pub groups_done: u64,
    pub groups_total: u64,
    pub files_hashed: u64,
    pub bytes_read: u64,
}

/// Somewhere events can go: the Tauri app, or a recorder in tests.
pub trait Emit: Send + Sync {
    /// Send one event. Failures are dropped: a closed window must not stop a scan.
    fn emit_json(&self, event: &str, payload: serde_json::Value);
}

/// Serialize and send. Serialization of these plain structs cannot fail; if it
/// ever did, the event is dropped rather than crashing the scan.
pub fn emit<T: Serialize>(sink: &dyn Emit, event: &str, payload: &T) {
    if let Ok(value) = serde_json::to_value(payload) {
        sink.emit_json(event, value);
    }
}

impl<R: tauri::Runtime> Emit for tauri::AppHandle<R> {
    fn emit_json(&self, event: &str, payload: serde_json::Value) {
        use tauri::Emitter;
        let _ = self.emit(event, payload);
    }
}

/// Lets one event through per interval.
#[derive(Debug)]
pub struct Throttle {
    every: Duration,
    last: Mutex<Option<Instant>>,
}

impl Throttle {
    pub fn new(every: Duration) -> Self {
        Self {
            every,
            last: Mutex::new(None),
        }
    }

    /// Whether an event may go now; records it if so.
    pub fn ready(&self) -> bool {
        self.ready_at(Instant::now())
    }

    fn ready_at(&self, now: Instant) -> bool {
        let Ok(mut last) = self.last.lock() else {
            return false;
        };
        match *last {
            Some(previous) if now.duration_since(previous) < self.every => false,
            _ => {
                *last = Some(now);
                true
            }
        }
    }
}

#[cfg(test)]
pub mod testing {
    use std::sync::Mutex;

    use super::Emit;

    /// Records events for assertions.
    #[derive(Debug, Default)]
    pub struct Recorder {
        pub events: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl Recorder {
        pub fn named(&self, event: &str) -> Vec<serde_json::Value> {
            self.events
                .lock()
                .map(|events| {
                    events
                        .iter()
                        .filter(|(name, _)| name == event)
                        .map(|(_, payload)| payload.clone())
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    impl Emit for Recorder {
        fn emit_json(&self, event: &str, payload: serde_json::Value) {
            if let Ok(mut events) = self.events.lock() {
                events.push((event.to_owned(), payload));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_throttle_spaces_events_out() {
        let throttle = Throttle::new(Duration::from_millis(100));
        let start = Instant::now();
        assert!(throttle.ready_at(start));
        assert!(!throttle.ready_at(start + Duration::from_millis(50)));
        assert!(!throttle.ready_at(start + Duration::from_millis(99)));
        assert!(throttle.ready_at(start + Duration::from_millis(100)));
        assert!(!throttle.ready_at(start + Duration::from_millis(150)));
    }

    #[test]
    fn payloads_are_camel_case() {
        let value = serde_json::to_value(ScanDoneEvent {
            scan_id: 3,
            status: ScanStatus::Complete,
            duration_ms: 12,
        })
        .expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({ "scanId": 3, "status": "complete", "durationMs": 12 })
        );
    }
}

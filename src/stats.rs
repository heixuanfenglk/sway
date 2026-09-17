use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const RECENT_LIMIT: usize = 12;

#[derive(Debug, Clone)]
pub struct FlowEvent {
    pub proto: String,
    pub target: String,
    pub at: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct TrafficSnapshot {
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub active_conns: u64,
    pub total_conns: u64,
    pub failed_conns: u64,
    pub uptime_secs: u64,
    pub recent: Vec<FlowEvent>,
}

#[derive(Debug, Default)]
pub struct TrafficStats {
    bytes_up: AtomicU64,
    bytes_down: AtomicU64,
    active_conns: AtomicU64,
    total_conns: AtomicU64,
    failed_conns: AtomicU64,
    recent: Mutex<VecDeque<FlowEvent>>,
    connected_at: Mutex<Option<Instant>>,
}

impl TrafficStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn mark_connected(&self) {
        if let Ok(mut t) = self.connected_at.lock() {
            *t = Some(Instant::now());
        }
    }

    pub fn add_up(&self, n: u64) {
        self.bytes_up.fetch_add(n, Ordering::Relaxed);
    }

    pub fn add_down(&self, n: u64) {
        self.bytes_down.fetch_add(n, Ordering::Relaxed);
    }

    pub fn begin_flow(&self, proto: &str, target: &str) {
        self.active_conns.fetch_add(1, Ordering::Relaxed);
        self.total_conns.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut q) = self.recent.lock() {
            q.push_front(FlowEvent {
                proto: proto.to_string(),
                target: target.to_string(),
                at: Instant::now(),
            });
            while q.len() > RECENT_LIMIT {
                q.pop_back();
            }
        }
    }

    pub fn end_flow(&self) {
        let _ = self
            .active_conns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    pub fn fail_flow(&self) {
        self.failed_conns.fetch_add(1, Ordering::Relaxed);
        let _ = self
            .active_conns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    pub fn snapshot(&self) -> TrafficSnapshot {
        let uptime_secs = self
            .connected_at
            .lock()
            .ok()
            .and_then(|g| *g)
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let recent = self
            .recent
            .lock()
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default();
        TrafficSnapshot {
            bytes_up: self.bytes_up.load(Ordering::Relaxed),
            bytes_down: self.bytes_down.load(Ordering::Relaxed),
            active_conns: self.active_conns.load(Ordering::Relaxed),
            total_conns: self.total_conns.load(Ordering::Relaxed),
            failed_conns: self.failed_conns.load(Ordering::Relaxed),
            uptime_secs,
            recent,
        }
    }
}

pub fn format_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let v = n as f64;
    if v >= GB {
        format!("{:.2} GB", v / GB)
    } else if v >= MB {
        format!("{:.2} MB", v / MB)
    } else if v >= KB {
        format!("{:.1} KB", v / KB)
    } else {
        format!("{n} B")
    }
}

pub fn format_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 0.0 {
        return "0 B/s".into();
    }
    format_bytes(bytes_per_sec as u64) + "/s"
}

pub fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

//! Small shared helpers: node-id generation, clock, byte formatting.
//!
//! Randomness and clock reads are confined to this module (the "UI edge"
//! sanctioned by the architecture contract) — nothing here is used during
//! chain replay or graph evaluation.

use mantis_graph::NodeId;
use std::sync::atomic::{AtomicU64, Ordering};

/// Fallback counter so id generation still works (uniquely within this
/// process) if the OS entropy source is unavailable.
static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Fresh random 128-bit node id, never zero. Generated at the UI edge and
/// recorded inside the `AddNode` op — replay never generates ids.
pub fn new_node_id() -> NodeId {
    let mut buf = [0u8; 16];
    for _ in 0..4 {
        if getrandom::getrandom(&mut buf).is_ok() {
            let id = u128::from_le_bytes(buf);
            if id != 0 {
                return NodeId(id);
            }
        }
    }
    // Entropy source failed (or produced zero four times): fall back to a
    // process-unique counter placed in the high bits so ids stay distinct.
    let n = FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
    NodeId(((n as u128) << 64) | 0xfa11_bacc)
}

/// Wall-clock milliseconds since the unix epoch (native).
#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Wall-clock milliseconds since the unix epoch (browser `Date.now()`).
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// Human-readable byte count ("482 B", "3.2 KB", "1.7 MB").
pub fn format_bytes(bytes: usize) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_unique_and_nonzero() {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..1000 {
            let id = new_node_id();
            assert_ne!(id.0, 0, "node id must never be zero");
            assert!(seen.insert(id), "duplicate node id generated: {id}");
        }
    }

    #[test]
    fn node_id_hex_round_trip() {
        let id = new_node_id();
        assert_eq!(NodeId::from_hex(&id.to_hex()), Some(id));
    }

    #[test]
    fn format_bytes_ranges() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}

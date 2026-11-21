//! Event loop and global probe for Wayland clients running on their own
//! connection and thread.

use std::os::fd::{AsFd, AsRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use tracing::warn;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

const POLL_TIMEOUT_MS: i32 = 500;

/// Reconnect backoff, mirroring the IPC backends (`niri.rs`, `sway.rs`).
pub(super) const RECONNECT_INITIAL_MS: u64 = 1000;
pub(super) const RECONNECT_MAX_MS: u64 = 30000;
const RECONNECT_MULTIPLIER: f64 = 1.5;

/// Sleep for the current delay and grow it, or stop if `running` was cleared.
/// Returns whether the caller should attempt a reconnect.
pub(super) fn reconnect_backoff(running: &AtomicBool, delay_ms: &mut u64) -> bool {
    if !running.load(Ordering::SeqCst) {
        return false;
    }
    thread::sleep(Duration::from_millis(*delay_ms));
    *delay_ms =
        ((*delay_ms as f64) * RECONNECT_MULTIPLIER).min(RECONNECT_MAX_MS as f64) as u64;
    running.load(Ordering::SeqCst)
}

/// Dispatch `queue` until `running` is cleared or the connection fails.
///
/// Returns `false` if it stopped because of a terminal error rather than a
/// requested stop.
pub(super) fn run<S>(
    queue: &mut EventQueue<S>,
    state: &mut S,
    running: &AtomicBool,
    label: &str,
) -> bool {
    while running.load(Ordering::SeqCst) {
        if let Err(e) = queue.dispatch_pending(state) {
            warn!("{label} dispatch failed: {e}");
            return false;
        }
        if let Err(e) = queue.flush() {
            warn!("{label} flush failed: {e}");
            return false;
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut pfd = libc::pollfd {
            fd: guard.connection_fd().as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, length 1.
        let ready = unsafe { libc::poll(&mut pfd, 1, POLL_TIMEOUT_MS) };
        if ready <= 0 {
            continue; // timeout (re-check `running`) or EINTR; guard is dropped
        }
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                warn!("{label} read failed: {e}");
                return false;
            }
        }
    }
    true
}

/// Whether the compositor advertises any of `interfaces` (one roundtrip).
pub(super) fn advertises_any(interfaces: &[&str]) -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _queue)) = registry_queue_init::<ProbeState>(&conn) else {
        return false;
    };
    globals.contents().with_list(|list| {
        list.iter()
            .any(|g| interfaces.contains(&g.interface.as_str()))
    })
}

struct ProbeState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ProbeState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_respects_stop() {
        let running = AtomicBool::new(true);
        let mut delay = 2;
        assert!(reconnect_backoff(&running, &mut delay));
        assert_eq!(delay, 3);

        running.store(false, Ordering::SeqCst);
        assert!(!reconnect_backoff(&running, &mut delay));
        // A cleared flag must not sleep or grow the delay.
        assert_eq!(delay, 3);
    }
}

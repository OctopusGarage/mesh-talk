//! Network.framework cost flags; no SSID access or hotspot-name heuristics.
use super::*;
use block2::RcBlock;
use std::{ffi::c_void, ptr::NonNull};

#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    fn nw_path_monitor_create() -> *mut c_void;
    fn nw_path_monitor_set_update_handler(monitor: *mut c_void, block: *const c_void);
    fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: *mut c_void);
    fn nw_path_monitor_start(monitor: *mut c_void);
    fn nw_path_monitor_cancel(monitor: *mut c_void);
    fn nw_path_get_status(path: *mut c_void) -> i32;
    fn nw_path_is_expensive(path: *mut c_void) -> bool;
    fn nw_path_is_constrained(path: *mut c_void) -> bool;
    fn nw_release(object: *mut c_void);
}
unsafe extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
}

struct Monitor(NonNull<c_void>);
// SAFETY: Network.framework monitor objects support cross-thread use. This owner
// only cancels/releases its +1 reference; callbacks own only a watch sender.
unsafe impl Send for Monitor {}
impl Drop for Monitor {
    fn drop(&mut self) {
        // SAFETY: create returned this owned reference; cancellation is safe while
        // a callback completes, and that callback never borrows the Node or owner.
        unsafe {
            nw_path_monitor_cancel(self.0.as_ptr());
            nw_release(self.0.as_ptr());
        }
    }
}

pub(super) async fn observe(node: Arc<Node>) {
    let Some((monitor, mut changes)) = start_monitor() else {
        return;
    };
    while changes.changed().await.is_ok() {
        if let Some(cost) = *changes.borrow_and_update() {
            node.set_network_cost(cost);
        }
    }
    drop(monitor);
}

fn start_monitor() -> Option<(Monitor, tokio::sync::watch::Receiver<Option<NetworkCost>>)> {
    // None distinguishes waiting for the native callback from an observed
    // unknown cost (for example, an offline path). Both remain protected.
    let (updates, changes) = tokio::sync::watch::channel(None);
    // SAFETY: the callback receives a borrowed nw_path valid for this invocation.
    // Network.framework copies the block before start and retains its queue.
    let monitor = unsafe {
        let raw = NonNull::new(nw_path_monitor_create())?;
        let monitor = Monitor(raw);
        let handler = RcBlock::new(move |path: *mut c_void| {
            if path.is_null() {
                return;
            }
            let cost = Some(classify(
                nw_path_get_status(path) == 1,
                nw_path_is_expensive(path) || nw_path_is_constrained(path),
            ));
            updates.send_if_modified(|old| {
                if *old == cost {
                    false
                } else {
                    *old = cost;
                    true
                }
            });
        });
        nw_path_monitor_set_update_handler(raw.as_ptr(), RcBlock::as_ptr(&handler).cast());
        nw_path_monitor_set_queue(raw.as_ptr(), dispatch_get_global_queue(0, 0));
        nw_path_monitor_start(raw.as_ptr());
        monitor
    };
    Some((monitor, changes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_monitor_reports_an_initial_observation() {
        let (monitor, mut changes) = start_monitor().expect("native path monitor");
        tokio::time::timeout(std::time::Duration::from_secs(5), changes.changed())
            .await
            .expect("native path callback deadline")
            .expect("monitor is alive");
        assert!(changes.borrow_and_update().is_some());
        drop(monitor);
    }
}

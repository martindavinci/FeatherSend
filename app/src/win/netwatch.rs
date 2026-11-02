//! Network change hints for the backend. `NotifyAddrChange` (IP Helper,
//! Windows 2000 and later) blocks a small thread until an IPv4 address is
//! added or removed; the backend debounces the bursts.

use std::time::Duration;
use windows_sys::Win32::NetworkManagement::IpHelper::NotifyAddrChange;

pub fn spawn(on_change: impl Fn() + Send + 'static) {
    let started = std::thread::Builder::new().name("netwatch".into()).stack_size(64 * 1024).spawn(
        move || {
            loop {
                // Both pointers null: a synchronous wait.
                let status = unsafe { NotifyAddrChange(std::ptr::null_mut(), std::ptr::null()) };
                if status == 0 {
                    on_change();
                } else {
                    tracing::warn!("NotifyAddrChange failed ({status}); polling only");
                    std::thread::sleep(Duration::from_secs(300));
                }
            }
        },
    );
    if let Err(e) = started {
        tracing::warn!("cannot watch address changes: {e}");
    }
}

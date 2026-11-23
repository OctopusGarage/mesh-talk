//! Windows interface notifications invalidate the local discovery inventory.

use std::{ffi::c_void, io, ptr};
use tokio::sync::mpsc;
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, HANDLE},
    NetworkManagement::IpHelper::{
        CancelMibChangeNotify2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE,
        MIB_UNICASTIPADDRESS_ROW, NotifyIpInterfaceChange, NotifyUnicastIpAddressChange,
    },
    Networking::WinSock::AF_UNSPEC,
};

pub(super) struct InterfaceEvents {
    wake: mpsc::Receiver<()>,
    _interface: Registration,
    _address: Registration,
}

impl InterfaceEvents {
    pub(super) fn subscribe() -> io::Result<Self> {
        let (send, wake) = mpsc::channel(1);
        let interface = Registration::new(send.clone(), |context, handle| {
            // SAFETY: Registration owns the callback context until cancellation.
            unsafe {
                NotifyIpInterfaceChange(AF_UNSPEC, Some(interface_changed), context, false, handle)
            }
        })?;
        let address = Registration::new(send, |context, handle| {
            // SAFETY: Registration owns the callback context until cancellation.
            unsafe {
                NotifyUnicastIpAddressChange(
                    AF_UNSPEC,
                    Some(address_changed),
                    context,
                    false,
                    handle,
                )
            }
        })?;
        Ok(Self {
            wake,
            _interface: interface,
            _address: address,
        })
    }

    pub(super) async fn next(&mut self) -> io::Result<()> {
        self.wake.recv().await.ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "interface notifications closed")
        })
    }
}

struct Registration {
    handle: HANDLE,
    context: Option<Box<mpsc::Sender<()>>>,
}

// SAFETY: The IP Helper registration handle can be deregistered on another
// thread. The callback only accesses the separately allocated, Sync sender.
unsafe impl Send for Registration {}

impl Registration {
    fn new(
        send: mpsc::Sender<()>,
        register: impl FnOnce(*const c_void, &mut HANDLE) -> u32,
    ) -> io::Result<Self> {
        let context = Box::new(send);
        let mut handle = ptr::null_mut();
        let status = register((&raw const *context).cast(), &mut handle);
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self {
            handle,
            context: Some(context),
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: The handle belongs to this registration. The callback never
        // acquires an application lock or waits on this task during cancellation.
        let status = unsafe { CancelMibChangeNotify2(self.handle) };
        if status != ERROR_SUCCESS {
            // A failed cancellation cannot prove Windows has stopped using the
            // pointer. Keep the small callback allocation valid in that case.
            if let Some(context) = self.context.take() {
                Box::leak(context);
            }
            log::warn!("Could not cancel interface change notification: {status}");
        }
    }
}

unsafe extern "system" fn interface_changed(
    context: *const c_void,
    _: *const MIB_IPINTERFACE_ROW,
    _: MIB_NOTIFICATION_TYPE,
) {
    // SAFETY: The API invokes this with its live registration context.
    unsafe { signal(context) };
}

unsafe extern "system" fn address_changed(
    context: *const c_void,
    _: *const MIB_UNICASTIPADDRESS_ROW,
    _: MIB_NOTIFICATION_TYPE,
) {
    // SAFETY: The API invokes this with its live registration context.
    unsafe { signal(context) };
}

unsafe fn signal(context: *const c_void) {
    // SAFETY: Registration keeps the sender allocated until cancellation.
    let send = unsafe { &*context.cast::<mpsc::Sender<()>>() };
    let _ = send.try_send(());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn interface_events_accept_os_callbacks() {
        let (send, mut wake) = mpsc::channel(1);
        let registration = Registration::new(send, |context, handle| {
            // SAFETY: The registration owns its callback context. This test
            // requests an initial OS notification without changing an adapter.
            unsafe {
                NotifyUnicastIpAddressChange(
                    AF_UNSPEC,
                    Some(address_changed),
                    context,
                    true,
                    handle,
                )
            }
        })
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), wake.recv())
            .await
            .expect("Windows did not deliver the initial notification")
            .expect("callback context closed before notification");
        drop(registration);
        assert_eq!(wake.recv().await, None);
    }

    #[tokio::test]
    async fn interface_events_coalesce_both_callbacks_and_unregister_on_drop() {
        let mut events = InterfaceEvents::subscribe().unwrap();
        let interface = events._interface.context.as_deref().unwrap();
        let address = events._address.context.as_deref().unwrap();
        // SAFETY: Both contexts are owned by the live registrations. The
        // callbacks intentionally ignore the OS row payloads.
        unsafe {
            interface_changed(
                (interface as *const mpsc::Sender<()>).cast(),
                ptr::null(),
                0,
            );
            address_changed((address as *const mpsc::Sender<()>).cast(), ptr::null(), 0);
        }
        assert_eq!(events.wake.len(), 1);
        events.next().await.unwrap();
        drop(events);
        // A new runtime owner can register after the previous owner retires.
        drop(InterfaceEvents::subscribe().unwrap());
    }
}

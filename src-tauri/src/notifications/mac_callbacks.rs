//! Native completion adapters retain reply ownership, never notification text.
use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::NSError;
use std::sync::mpsc::SyncSender;

pub(super) fn submission() -> RcBlock<dyn Fn(*mut NSError)> {
    RcBlock::new(|error: *mut NSError| {
        if let Some(error) = unsafe { error.as_ref() } {
            // Native descriptions can include message content.
            log::warn!(
                "Native notification submission failed (code {})",
                error.code()
            );
        }
    })
}

pub(super) fn authorization(sender: SyncSender<bool>) -> RcBlock<dyn Fn(Bool, *mut NSError)> {
    RcBlock::new(move |_granted: Bool, error: *mut NSError| {
        if let Some(error) = unsafe { error.as_ref() } {
            log::warn!(
                "Native notification authorization failed (domain {}, code {})",
                error.domain(),
                error.code()
            );
        }
        // Completion is not a grant: the caller must read fresh OS settings.
        let _ = sender.send(error.is_null());
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::rc::Retained;
    use objc2_foundation::NSString;
    use std::{ptr::null_mut, sync::mpsc};

    fn native_error() -> Retained<NSError> {
        // No dictionary values are passed; the borrowed native error lives
        // throughout every callback invocation below.
        unsafe {
            NSError::errorWithDomain_code_userInfo(
                &NSString::from_str("MeshTalkNotificationCallbackFixture"),
                37,
                None,
            )
        }
    }

    #[test]
    fn authorization_completion_is_not_a_permission_grant_and_retires_its_sender() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let reply = authorization(sender);
        reply.call((Bool::NO, null_mut()));
        assert!(
            receiver.try_recv().unwrap(),
            "successful completion requires a fresh settings read"
        );
        let error = native_error();
        reply.call((Bool::YES, Retained::as_ptr(&error).cast_mut()));
        assert!(
            !receiver.try_recv().unwrap(),
            "an error cannot become a successful completion"
        );
        drop(reply);
        assert_eq!(receiver.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    }

    #[test]
    fn late_authorization_completion_after_observer_retirement_does_not_wait() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let reply = authorization(sender);
        drop(receiver);
        reply.call((Bool::NO, null_mut()));
        let error = native_error();
        reply.call((Bool::YES, Retained::as_ptr(&error).cast_mut()));
    }

    #[test]
    fn submission_completion_accepts_nil_and_borrowed_native_errors() {
        let reply = submission();
        reply.call((null_mut(),));
        let error = native_error();
        reply.call((Retained::as_ptr(&error).cast_mut(),));
    }
}

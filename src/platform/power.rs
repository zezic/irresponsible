//! Sleep invalidates probe leases and briefly allows device streams to quiesce.
use crate::{audio::Cancellation, error::Result};

#[cfg(target_os = "macos")]
pub struct SleepMonitor {
    center: objc2::rc::Retained<objc2_foundation::NSNotificationCenter>,
    observers: Vec<
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
    >,
}
#[cfg(target_os = "macos")]
impl SleepMonitor {
    pub fn new(cancellation: Cancellation) -> Result<Self> {
        use objc2_app_kit::{
            NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification,
        };
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        let sleeping = cancellation.clone();
        let sleep = block2::RcBlock::new(
            move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| {
                sleeping.prepare_for_sleep()
            },
        );
        let wake = block2::RcBlock::new(
            move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| cancellation.wake(),
        );
        // SAFETY: the workspace is the documented sender, and both blocks
        // capture only Send/Sync atomic state. Delivery uses the posting thread.
        let observers = unsafe {
            vec![
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceWillSleepNotification),
                    Some(&workspace),
                    None,
                    &sleep,
                ),
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceDidWakeNotification),
                    Some(&workspace),
                    None,
                    &wake,
                ),
            ]
        };
        Ok(Self { center, observers })
    }
}
#[cfg(target_os = "macos")]
impl Drop for SleepMonitor {
    fn drop(&mut self) {
        // SAFETY: these tokens were registered with this exact center.
        unsafe {
            for observer in &self.observers {
                self.center.removeObserver((**observer).as_ref());
            }
        }
    }
}
#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn workspace_sleep_notification_invalidates_the_probe_lease() {
        use objc2_app_kit::{NSWorkspace, NSWorkspaceWillSleepNotification};
        let cancellation = crate::audio::Cancellation::new();
        let _monitor = super::SleepMonitor::new(cancellation.clone()).unwrap();
        let workspace = NSWorkspace::sharedWorkspace();
        // SAFETY: a process-local test notification with its documented sender.
        unsafe {
            workspace
                .notificationCenter()
                .postNotificationName_object(NSWorkspaceWillSleepNotification, Some(&workspace));
        }
        assert!(cancellation.is_cancelled());
    }
}
#[cfg(not(target_os = "macos"))]
pub struct SleepMonitor;
#[cfg(not(target_os = "macos"))]
impl SleepMonitor {
    pub fn new(_: Cancellation) -> Result<Self> {
        Ok(Self)
    }
}

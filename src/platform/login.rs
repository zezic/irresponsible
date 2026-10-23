//! macOS owns login-item registration; application settings do not mirror it.
use crate::error::{Result, invalid};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Disabled,
    Enabled,
    ApprovalRequired,
    NotFound,
    Unbundled,
    Unsupported,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Enable,
    Disable,
}
impl Status {
    pub fn requested(self) -> bool {
        matches!(self, Self::Enabled | Self::ApprovalRequired)
    }
    pub fn change(self) -> Option<Change> {
        match self {
            Self::Disabled | Self::NotFound => Some(Change::Enable),
            Self::Enabled | Self::ApprovalRequired => Some(Change::Disable),
            Self::Unbundled | Self::Unsupported => None,
        }
    }
    pub fn needs_settings(self) -> bool {
        matches!(self, Self::ApprovalRequired)
    }
    pub fn caption(self) -> &'static str {
        match self {
            Self::Disabled | Self::NotFound => "Off",
            Self::Enabled => "Enabled",
            Self::ApprovalRequired => "Allow IRresponsible in Login Items.",
            Self::Unbundled => "Open IRresponsible.app to change this.",
            Self::Unsupported => "Available on macOS.",
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2_foundation::NSBundle;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    fn bundled() -> bool {
        let bundle = NSBundle::mainBundle();
        bundle.bundleIdentifier().is_some()
            && bundle
                .bundleURL()
                .pathExtension()
                .is_some_and(|extension| extension.to_string() == "app")
    }
    fn decode(status: SMAppServiceStatus) -> Result<Status> {
        match status {
            SMAppServiceStatus::NotRegistered => Ok(Status::Disabled),
            SMAppServiceStatus::Enabled => Ok(Status::Enabled),
            SMAppServiceStatus::RequiresApproval => Ok(Status::ApprovalRequired),
            SMAppServiceStatus::NotFound => Ok(Status::NotFound),
            other => Err(invalid(format!(
                "Unknown macOS login-item status: {}",
                other.0
            ))),
        }
    }
    pub fn status() -> Result<Status> {
        if !bundled() {
            return Ok(Status::Unbundled);
        }
        // SAFETY: SMAppService is available on the app's minimum macOS version (13).
        decode(unsafe { SMAppService::mainAppService().status() })
    }
    pub fn change(change: Change) -> Result<Status> {
        if !bundled() {
            return Err(invalid(Status::Unbundled.caption()));
        }
        // SAFETY: this is the running main app's service, on supported macOS.
        let service = unsafe { SMAppService::mainAppService() };
        // SAFETY: these methods have no additional preconditions for a main app.
        unsafe {
            match change {
                Change::Enable => service.registerAndReturnError(),
                Change::Disable => service.unregisterAndReturnError(),
            }
        }
        .map_err(|error| invalid(error.localizedDescription().to_string()))?;
        decode(unsafe { service.status() })
    }
    pub fn open_settings() -> Result<()> {
        // SAFETY: the class method opens Apple's Login Items pane on macOS 13+.
        unsafe {
            SMAppService::openSystemSettingsLoginItems();
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn native_states_preserve_approval_and_reject_unknown_statuses() {
            assert_eq!(
                decode(SMAppServiceStatus::NotRegistered).unwrap(),
                Status::Disabled
            );
            assert_eq!(
                decode(SMAppServiceStatus::Enabled).unwrap(),
                Status::Enabled
            );
            assert_eq!(
                decode(SMAppServiceStatus::RequiresApproval).unwrap(),
                Status::ApprovalRequired
            );
            assert_eq!(
                decode(SMAppServiceStatus::NotFound).unwrap(),
                Status::NotFound
            );
            assert!(decode(SMAppServiceStatus(99)).is_err());
            assert_eq!(Status::NotFound.change(), Some(Change::Enable));
            assert!(!Status::NotFound.requested() && !Status::NotFound.needs_settings());
            assert!(
                Status::ApprovalRequired.requested() && Status::ApprovalRequired.needs_settings()
            );
        }
    }
}
#[cfg(target_os = "macos")]
pub use mac::{change, open_settings, status};
#[cfg(not(target_os = "macos"))]
pub fn status() -> Result<Status> {
    Ok(Status::Unsupported)
}
#[cfg(not(target_os = "macos"))]
pub fn change(_: Change) -> Result<Status> {
    Err(invalid(Status::Unsupported.caption()))
}
#[cfg(not(target_os = "macos"))]
pub fn open_settings() -> Result<()> {
    Err(invalid(Status::Unsupported.caption()))
}

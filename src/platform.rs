//! macOS audio routing, device notifications, and login-item management.
pub mod login;
mod power;
use crate::{
    domain::DeviceUid,
    error::{Result, invalid},
};
pub use power::SleepMonitor;

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use core_foundation::{
        base::TCFType,
        string::{CFString, CFStringRef},
    };
    use coreaudio_sys::*;
    use std::{
        ffi::c_void,
        mem::size_of,
        ptr,
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
    };

    fn address(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        }
    }
    fn checked(status: i32) -> Result<()> {
        if status == 0 {
            Ok(())
        } else {
            Err(invalid(format!("Core Audio property error {status}")))
        }
    }
    fn read<T: Copy + Default>(device: u32, selector: u32) -> Result<T> {
        let mut result = T::default();
        let mut bytes = size_of::<T>() as u32;
        // SAFETY: the selector-specific callers request the documented scalar type.
        checked(unsafe {
            AudioObjectGetPropertyData(
                device,
                &address(selector),
                0,
                ptr::null(),
                &mut bytes,
                (&mut result as *mut T).cast(),
            )
        })?;
        Ok(result)
    }
    fn default_device() -> Result<u32> {
        read(
            kAudioObjectSystemObject,
            kAudioHardwarePropertyDefaultOutputDevice,
        )
    }
    fn set_default(device: u32) -> Result<()> {
        // SAFETY: default output is an AudioDeviceID, with global scope.
        checked(unsafe {
            AudioObjectSetPropertyData(
                kAudioObjectSystemObject,
                &address(kAudioHardwarePropertyDefaultOutputDevice),
                0,
                ptr::null(),
                size_of::<u32>() as u32,
                (&device as *const u32).cast(),
            )
        })
    }
    fn uid(device: u32) -> Result<String> {
        let mut string: CFStringRef = ptr::null();
        let mut bytes = size_of::<CFStringRef>() as u32;
        // SAFETY: this property returns a retained CFString per Core Audio's ownership contract.
        checked(unsafe {
            AudioObjectGetPropertyData(
                device,
                &address(kAudioDevicePropertyDeviceUID),
                0,
                ptr::null(),
                &mut bytes,
                (&mut string as *mut CFStringRef).cast(),
            )
        })?;
        if string.is_null() {
            return Err(invalid("Device returned an empty UID"));
        }
        Ok(unsafe { CFString::wrap_under_create_rule(string) }.to_string())
    }
    fn object_ids(object: u32, selector: u32) -> Result<Vec<u32>> {
        let property = address(selector);
        let mut bytes = 0;
        // SAFETY: Core Audio reports the byte length of its AudioDeviceID array.
        checked(unsafe {
            AudioObjectGetPropertyDataSize(object, &property, 0, ptr::null(), &mut bytes)
        })?;
        if !(bytes as usize).is_multiple_of(size_of::<u32>()) {
            return Err(invalid("Core Audio returned a malformed object ID array"));
        }
        let mut devices = vec![0; bytes as usize / size_of::<u32>()];
        checked(unsafe {
            AudioObjectGetPropertyData(
                object,
                &property,
                0,
                ptr::null(),
                &mut bytes,
                devices.as_mut_ptr().cast(),
            )
        })?;
        devices.truncate(bytes as usize / size_of::<u32>());
        Ok(devices)
    }
    fn devices() -> Result<Vec<u32>> {
        object_ids(kAudioObjectSystemObject, kAudioHardwarePropertyDevices)
    }
    fn device_by_uid(selected: &str) -> Result<u32> {
        devices()?
            .into_iter()
            .find(|&device| uid(device).is_ok_and(|uid| uid == selected))
            .ok_or_else(|| invalid(format!("Core Audio device UID {selected} is unavailable")))
    }

    pub fn output_is(uid: &DeviceUid) -> Result<bool> {
        Ok(default_device()? == device_by_uid(uid.id().id())?)
    }
    pub fn is_virtual_device(uid: &DeviceUid) -> Result<bool> {
        let device = device_by_uid(uid.id().id())?;
        let transport: u32 = read(device, kAudioDevicePropertyTransportType)?;
        Ok(transport == kAudioDeviceTransportTypeVirtual)
    }

    struct PropertyListener<T> {
        value: Option<Box<T>>,
        object: u32,
        property: AudioObjectPropertyAddress,
        callback: AudioObjectPropertyListenerProc,
    }
    impl<T> PropertyListener<T> {
        fn new(selector: u32, value: T, callback: AudioObjectPropertyListenerProc) -> Result<Self> {
            Self::on(kAudioObjectSystemObject, address(selector), value, callback)
        }
        fn on(
            object: u32,
            property: AudioObjectPropertyAddress,
            value: T,
            callback: AudioObjectPropertyListenerProc,
        ) -> Result<Self> {
            let mut value = Box::new(value);
            let context = (&mut *value as *mut T).cast();
            // SAFETY: the heap address remains stable until the listener is removed.
            checked(unsafe {
                AudioObjectAddPropertyListener(object, &property, callback, context)
            })?;
            Ok(Self {
                value: Some(value),
                object,
                property,
                callback,
            })
        }
    }
    impl<T> Drop for PropertyListener<T> {
        fn drop(&mut self) {
            let context =
                (&mut **self.value.as_mut().expect("registered context") as *mut T).cast();
            // SAFETY: the context still exists while Core Audio removes the callback.
            let status = unsafe {
                AudioObjectRemovePropertyListener(
                    self.object,
                    &self.property,
                    self.callback,
                    context,
                )
            };
            if status != 0 {
                // Retain the callback context when the service cannot confirm
                // removal, so a late notification cannot access freed memory.
                let retained = self.value.take().expect("registered context");
                std::mem::forget(retained);
            }
        }
    }

    struct Watch {
        owned_device: u32,
        lost: AtomicBool,
    }
    unsafe extern "C" fn changed(
        _: u32,
        _: u32,
        _: *const AudioObjectPropertyAddress,
        context: *mut c_void,
    ) -> i32 {
        // SAFETY: the boxed context outlives the registered listener.
        let watch = unsafe { &*context.cast::<Watch>() };
        if default_device().is_ok_and(|device| device != watch.owned_device) {
            watch.lost.store(true, Ordering::Relaxed);
        }
        0
    }
    pub struct DefaultOutputLease {
        previous_uid: String,
        listener: PropertyListener<Watch>,
    }
    impl DefaultOutputLease {
        pub fn acquire(capture: &DeviceUid) -> Result<Option<Self>> {
            let device = device_by_uid(capture.id().id())?;
            let previous = default_device()?;
            if previous == device {
                return Ok(None);
            }
            let previous_uid = uid(previous)?;
            let listener = PropertyListener::new(
                kAudioHardwarePropertyDefaultOutputDevice,
                Watch {
                    owned_device: device,
                    lost: AtomicBool::new(false),
                },
                Some(changed),
            )?;
            set_default(device)?;
            Ok(Some(Self {
                previous_uid,
                listener,
            }))
        }
        pub fn owns(&self) -> bool {
            !self
                .listener
                .value
                .as_ref()
                .expect("registered output lease")
                .lost
                .load(Ordering::Relaxed)
        }
        pub fn restore(&mut self) -> Result<()> {
            let watch = self
                .listener
                .value
                .as_ref()
                .expect("registered ownership listener");
            if !watch.lost.swap(true, Ordering::Relaxed) && default_device()? == watch.owned_device
            {
                let previous = device_by_uid(&self.previous_uid)?;
                set_default(previous)?;
            }
            Ok(())
        }
    }
    impl Drop for DefaultOutputLease {
        fn drop(&mut self) {
            let _ = self.restore();
        }
    }
    unsafe extern "C" fn devices_changed(
        _: u32,
        _: u32,
        _: *const AudioObjectPropertyAddress,
        context: *mut c_void,
    ) -> i32 {
        // SAFETY: PropertyListener owns this counter until callback removal.
        unsafe { &*context.cast::<AtomicU64>() }.fetch_add(1, Ordering::Relaxed);
        0
    }
    struct RevisionWatch {
        listener: PropertyListener<AtomicU64>,
        observed: u64,
    }
    impl RevisionWatch {
        fn new(object: u32, property: AudioObjectPropertyAddress) -> Result<Self> {
            Ok(Self {
                listener: PropertyListener::on(
                    object,
                    property,
                    AtomicU64::new(0),
                    Some(devices_changed),
                )?,
                observed: 0,
            })
        }
        fn changed(&mut self) -> bool {
            let revision = self
                .listener
                .value
                .as_ref()
                .expect("registered property listener")
                .load(Ordering::Relaxed);
            let changed = revision != self.observed;
            self.observed = revision;
            changed
        }
    }
    pub struct DeviceMonitor {
        system: RevisionWatch,
        route: RevisionWatch,
        endpoints: Vec<RevisionWatch>,
        watched_devices: Vec<u32>,
    }
    impl DeviceMonitor {
        pub fn new() -> Result<Self> {
            Ok(Self {
                system: RevisionWatch::new(
                    kAudioObjectSystemObject,
                    address(kAudioHardwarePropertyDevices),
                )?,
                route: RevisionWatch::new(
                    kAudioObjectSystemObject,
                    address(kAudioHardwarePropertyDefaultOutputDevice),
                )?,
                endpoints: Vec::new(),
                watched_devices: Vec::new(),
            })
        }
        pub fn watch(&mut self, endpoints: &[&DeviceUid]) -> Result<()> {
            let mut devices = Vec::new();
            let mut watches = Vec::new();
            for endpoint in endpoints {
                let device = device_by_uid(endpoint.id().id())?;
                if devices.contains(&device) {
                    continue;
                }
                devices.push(device);
                for selector in [
                    kAudioDevicePropertyNominalSampleRate,
                    kAudioDevicePropertyDeviceIsAlive,
                ] {
                    watches.push(RevisionWatch::new(device, address(selector))?);
                }
                let control_list = address(kAudioObjectPropertyControlList);
                // SAFETY: this queries property presence without reading data.
                if unsafe { AudioObjectHasProperty(device, &control_list) } != 0 {
                    for control in object_ids(device, kAudioObjectPropertyControlList)? {
                        for selector in [
                            kAudioLevelControlPropertyScalarValue,
                            kAudioLevelControlPropertyDecibelValue,
                            kAudioBooleanControlPropertyValue,
                            kAudioSelectorControlPropertyCurrentItem,
                        ] {
                            let property = address(selector);
                            // SAFETY: control IDs came from this device; unsupported classes are skipped.
                            if unsafe { AudioObjectHasProperty(control, &property) } != 0 {
                                watches.push(RevisionWatch::new(control, property)?);
                            }
                        }
                    }
                }
                for scope in [
                    kAudioObjectPropertyScopeInput,
                    kAudioObjectPropertyScopeOutput,
                ] {
                    let mut property = address(kAudioDevicePropertyStreamConfiguration);
                    property.mScope = scope;
                    // SAFETY: the address is a valid selector/scope pair; no data is read.
                    if unsafe { AudioObjectHasProperty(device, &property) } != 0 {
                        watches.push(RevisionWatch::new(device, property)?);
                    }
                }
            }
            self.endpoints = watches;
            self.watched_devices = devices;
            Ok(())
        }
        pub fn changed(&mut self) -> bool {
            self.endpoints.iter_mut().fold(
                self.system.changed() | self.route.changed(),
                |changed, watch| watch.changed() || changed,
            )
        }
        /// Observe the selected path without treating unrelated hotplug as a fault.
        pub fn watched_changed(&mut self) -> Result<bool> {
            let inventory_changed = self.system.changed();
            let changed = self
                .endpoints
                .iter_mut()
                .fold(self.route.changed(), |changed, watch| {
                    watch.changed() || changed
                });
            if inventory_changed {
                let available = devices()?;
                return Ok(changed
                    || self
                        .watched_devices
                        .iter()
                        .any(|id| !available.contains(id)));
            }
            Ok(changed)
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::{DefaultOutputLease, DeviceMonitor, is_virtual_device, output_is};

#[cfg(not(target_os = "macos"))]
pub struct DefaultOutputLease;
#[cfg(not(target_os = "macos"))]
impl DefaultOutputLease {
    pub fn owns(&self) -> bool {
        true
    }
    pub fn acquire(_: &DeviceUid) -> Result<Option<Self>> {
        Err(invalid("System output selection is supported on macOS"))
    }
    pub fn restore(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
pub struct DeviceMonitor;
#[cfg(not(target_os = "macos"))]
impl DeviceMonitor {
    pub fn watch(&mut self, _: &[&DeviceUid]) -> Result<()> {
        Ok(())
    }
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
    pub fn changed(&mut self) -> bool {
        false
    }
    pub fn watched_changed(&mut self) -> Result<bool> {
        Ok(false)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn output_is(_: &DeviceUid) -> Result<bool> {
    Err(invalid("System output routing is supported on macOS"))
}

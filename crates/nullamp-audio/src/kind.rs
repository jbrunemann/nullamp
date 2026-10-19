//! What sort of output a device is, so the UI can tell real sound hardware from
//! software routing (BlackHole, Teams, Steam and similar).

/// How an output device connects to the system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputKind {
    #[default]
    Unknown,
    /// The computer's own speakers or headphone jack.
    BuiltIn,
    Usb,
    Bluetooth,
    /// A monitor or TV over HDMI or DisplayPort.
    Display,
    /// AirPlay, AVB and other network audio.
    Network,
    /// A sound card or interface on PCI, Thunderbolt or FireWire.
    Card,
    /// Software only: loopback drivers and aggregate devices.
    Virtual,
}

impl OutputKind {
    /// Real sound hardware, as opposed to a virtual device or one of unknown origin.
    pub fn is_hardware(self) -> bool {
        !matches!(self, OutputKind::Unknown | OutputKind::Virtual)
    }
}

#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals)] // CoreAudio's constants, matched as patterns
pub(crate) fn kind_of(id: &cpal::DeviceId, _device: &cpal::Device) -> OutputKind {
    use objc2_core_audio::*;
    match coreaudio_transport(id.id()) {
        Some(kAudioDeviceTransportTypeBuiltIn) => OutputKind::BuiltIn,
        Some(kAudioDeviceTransportTypeUSB) => OutputKind::Usb,
        Some(kAudioDeviceTransportTypeBluetooth | kAudioDeviceTransportTypeBluetoothLE) => OutputKind::Bluetooth,
        Some(kAudioDeviceTransportTypeHDMI | kAudioDeviceTransportTypeDisplayPort) => OutputKind::Display,
        Some(kAudioDeviceTransportTypeAirPlay | kAudioDeviceTransportTypeAVB) => OutputKind::Network,
        Some(kAudioDeviceTransportTypePCI | kAudioDeviceTransportTypeThunderbolt | kAudioDeviceTransportTypeFireWire) => OutputKind::Card,
        Some(kAudioDeviceTransportTypeVirtual | kAudioDeviceTransportTypeAggregate) => OutputKind::Virtual,
        _ => OutputKind::Unknown,
    }
}

/// CoreAudio's transport type of the device with this UID (cpal's id minus the host).
#[cfg(target_os = "macos")]
fn coreaudio_transport(uid: &str) -> Option<u32> {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use objc2_core_audio::*;
    use objc2_core_foundation::CFString;

    fn property<T: Default>(object: AudioObjectID, selector: u32, qualifier: Option<&*const CFString>) -> Option<T> {
        let address = AudioObjectPropertyAddress { mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain };
        let mut value = T::default();
        let mut size = size_of::<T>() as u32;
        let (q_size, q_data) = match qualifier {
            Some(q) => (size_of::<*const CFString>() as u32, q as *const _ as *const c_void),
            None => (0, std::ptr::null()),
        };
        // SAFETY: the address, size and output pointers are valid for the call, `value`
        // is `size` bytes, and the qualifier (when given) points at a live CFStringRef.
        let status = unsafe {
            AudioObjectGetPropertyData(object, NonNull::from(&address), q_size, q_data, NonNull::from(&mut size), NonNull::from(&mut value).cast())
        };
        (status == 0).then_some(value)
    }

    let uid = CFString::from_str(uid);
    let qualifier: *const CFString = &*uid;
    let device: AudioDeviceID = property(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyTranslateUIDToDevice, Some(&qualifier))?;
    if device == 0 {
        return None;
    }
    property(device, kAudioDevicePropertyTransportType, None)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn kind_of(_id: &cpal::DeviceId, device: &cpal::Device) -> OutputKind {
    use cpal::traits::DeviceTrait;
    use cpal::{DeviceType, InterfaceType};

    let Ok(desc) = device.description() else { return OutputKind::Unknown };
    if desc.device_type() == DeviceType::Virtual {
        return OutputKind::Virtual;
    }
    match desc.interface_type() {
        InterfaceType::BuiltIn => OutputKind::BuiltIn,
        InterfaceType::Usb => OutputKind::Usb,
        InterfaceType::Bluetooth => OutputKind::Bluetooth,
        InterfaceType::Hdmi => OutputKind::Display,
        InterfaceType::Network => OutputKind::Network,
        InterfaceType::Pci | InterfaceType::Thunderbolt | InterfaceType::FireWire => OutputKind::Card,
        _ => OutputKind::Unknown,
    }
}

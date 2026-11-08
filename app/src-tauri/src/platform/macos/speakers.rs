//! Mutes the default output device while recording. Devices without a mute
//! control are left alone.

use std::ffi::c_void;
use std::ptr::{NonNull, null};

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectHasProperty, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertySelector, AudioObjectSetPropertyData, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyMute, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyTranslateUIDToDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_core_foundation::{CFRetained, CFString};

const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

fn global(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn mute_address() -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyMute,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads a fixed-size property, optionally with qualifier data.
unsafe fn read<T: Copy>(
    object: AudioObjectID,
    mut address: AudioObjectPropertyAddress,
    qualifier: Option<(*const c_void, u32)>,
    mut value: T,
) -> Option<T> {
    let (qualifier, qualifier_size) = qualifier.unwrap_or((null(), 0));
    let mut size = size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut address),
            qualifier_size,
            qualifier,
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == 0).then_some(value)
}

fn set_mute(device: AudioObjectID, muted: bool) -> bool {
    let mut address = mute_address();
    let mut value = u32::from(muted);
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            NonNull::from(&mut address),
            0,
            null(),
            size_of::<u32>() as u32,
            NonNull::from(&mut value).cast(),
        )
    };
    status == 0
}

fn device_uid(device: AudioObjectID) -> Option<String> {
    let uid: *const CFString =
        unsafe { read(device, global(kAudioDevicePropertyDeviceUID), None, null())? };
    let uid = unsafe { CFRetained::from_raw(NonNull::new(uid.cast_mut())?) };
    Some(uid.to_string())
}

fn device_from_uid(uid: &str) -> Option<AudioObjectID> {
    let uid = CFString::from_str(uid);
    let reference: *const CFString = CFRetained::as_ptr(&uid).as_ptr();
    let device: AudioObjectID = unsafe {
        read(
            SYSTEM,
            global(kAudioHardwarePropertyTranslateUIDToDevice),
            Some((
                (&reference as *const *const CFString).cast(),
                size_of::<*const CFString>() as u32,
            )),
            0,
        )?
    };
    (device != 0).then_some(device)
}

pub fn mute() -> Option<String> {
    let device: AudioObjectID = unsafe {
        read(
            SYSTEM,
            global(kAudioHardwarePropertyDefaultOutputDevice),
            None,
            0,
        )?
    };
    let mut address = mute_address();
    if device == 0 || !unsafe { AudioObjectHasProperty(device, NonNull::from(&mut address)) } {
        return None;
    }
    let muted: u32 = unsafe { read(device, mute_address(), None, 0)? };
    if muted != 0 {
        return None;
    }

    let uid = device_uid(device)?;
    set_mute(device, true).then_some(uid)
}

pub fn unmute(device: &str) {
    if let Some(device) = device_from_uid(device) {
        set_mute(device, false);
    }
}

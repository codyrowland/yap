use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::core::{HSTRING, Result};

pub fn mute() -> Option<String> {
    with_com(|| unsafe {
        let device = enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let volume = endpoint_volume(&device)?;
        if volume.GetMute()?.as_bool() {
            return Ok(None);
        }

        let id = device.GetId()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        let Some(text) = text else {
            return Ok(None);
        };
        volume.SetMute(true, std::ptr::null())?;
        Ok(Some(text))
    })
    .flatten()
}

pub fn unmute(device: &str) {
    let device = HSTRING::from(device);
    let _ = with_com(move || unsafe {
        let device = enumerator()?.GetDevice(&device)?;
        endpoint_volume(&device)?.SetMute(false, std::ptr::null())
    });
}

unsafe fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

unsafe fn endpoint_volume(device: &IMMDevice) -> Result<IAudioEndpointVolume> {
    unsafe { device.Activate(CLSCTX_ALL, None) }
}

fn with_com<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Option<T> {
    std::thread::spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return None;
        }

        let result = work().ok();
        CoUninitialize();
        result
    })
    .join()
    .ok()
    .flatten()
}

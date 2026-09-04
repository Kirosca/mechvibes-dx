//! Audio device hotplug & default device change watcher (Windows WASAPI IMMNotificationClient).
//!
//! Listens for system-level audio endpoint changes (e.g. plugging/unplugging headphones,
//! connecting Bluetooth devices, or switching default output in the Windows volume tray)
//! and automatically switches the active audio stream to follow the new default device
//! with zero polling and zero latency.

#![allow(non_snake_case)]

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::ptr::null_mut;
    use std::sync::atomic::{ AtomicU32, AtomicU64, Ordering };
    use winapi::ctypes::c_void;
    use winapi::shared::guiddef::{ GUID, IID };
    use winapi::shared::minwindef::{ DWORD, ULONG };
    use winapi::shared::winerror::{ E_NOINTERFACE, HRESULT, S_OK };
    use winapi::shared::wtypes::PROPERTYKEY;
    use winapi::um::combaseapi::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use winapi::um::winnt::LPCWSTR;

    const CLSID_MM_DEVICE_ENUMERATOR: GUID = GUID {
        Data1: 0xBCDE0395,
        Data2: 0xE52F,
        Data3: 0x467C,
        Data4: [0x8E, 0x3D, 0xC4, 0x57, 0x92, 0x91, 0x69, 0x2E],
    };

    const IID_IMM_DEVICE_ENUMERATOR: GUID = GUID {
        Data1: 0xA95664D2,
        Data2: 0x9614,
        Data3: 0x4F35,
        Data4: [0xA7, 0x46, 0xDE, 0x8D, 0xB6, 0x36, 0x17, 0xE6],
    };

    const IID_IMM_NOTIFICATION_CLIENT: GUID = GUID {
        Data1: 0x7991EEC0,
        Data2: 0x3630,
        Data3: 0x4207,
        Data4: [0xB1, 0x0D, 0x6C, 0x0E, 0x87, 0x5C, 0x60, 0xCE],
    };

    const IID_I_UNKNOWN: GUID = GUID {
        Data1: 0x00000000,
        Data2: 0x0000,
        Data3: 0x0000,
        Data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };

    static LAST_TRIGGER: AtomicU64 = AtomicU64::new(0);

    fn trigger_device_reload() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let last = LAST_TRIGGER.swap(now, Ordering::SeqCst);
        if now.saturating_sub(last) < 150 {
            return; // Debounce rapid bursts
        }

        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(60));
            let config = crate::state::config_writer::current();
            if config.selected_audio_device.is_none() {
                crate::always_print!("🔊 [DeviceWatcher] System default audio device changed - switching output stream");
                crate::libs::audio::engine_handle().send(crate::libs::audio::AudioCommand::SwitchDevice(None));
                crate::state::ambiance::switch_ambiance_player_device(None);
            } else if let Some(ref selected_id) = config.selected_audio_device {
                crate::always_print!("🔊 [DeviceWatcher] Audio device state changed - reconnecting output stream");
                crate::libs::audio::engine_handle().send(crate::libs::audio::AudioCommand::SwitchDevice(Some(selected_id.clone())));
                crate::state::ambiance::switch_ambiance_player_device(Some(selected_id.clone()));
            }
        });
    }

    #[repr(C)]
    struct IMMNotificationClientVtbl {
        QueryInterface: unsafe extern "system" fn(
            this: *mut IMMNotificationClient,
            riid: *const IID,
            ppvObject: *mut *mut c_void,
        ) -> HRESULT,
        AddRef: unsafe extern "system" fn(this: *mut IMMNotificationClient) -> ULONG,
        Release: unsafe extern "system" fn(this: *mut IMMNotificationClient) -> ULONG,
        OnDeviceStateChanged: unsafe extern "system" fn(
            this: *mut IMMNotificationClient,
            pwstrDeviceId: LPCWSTR,
            dwNewState: DWORD,
        ) -> HRESULT,
        OnDeviceAdded: unsafe extern "system" fn(
            this: *mut IMMNotificationClient,
            pwstrDeviceId: LPCWSTR,
        ) -> HRESULT,
        OnDeviceRemoved: unsafe extern "system" fn(
            this: *mut IMMNotificationClient,
            pwstrDeviceId: LPCWSTR,
        ) -> HRESULT,
        OnDefaultDeviceChanged: unsafe extern "system" fn(
            this: *mut IMMNotificationClient,
            flow: DWORD,
            role: DWORD,
            pwstrDefaultDeviceId: LPCWSTR,
        ) -> HRESULT,
        OnPropertyValueChanged: unsafe extern "system" fn(
            this: *mut IMMNotificationClient,
            pwstrDeviceId: LPCWSTR,
            key: *const PROPERTYKEY,
        ) -> HRESULT,
    }

    #[repr(C)]
    struct IMMNotificationClient {
        lpVtbl: *const IMMNotificationClientVtbl,
        ref_count: AtomicU32,
    }

    unsafe extern "system" fn QueryInterface(
        this: *mut IMMNotificationClient,
        riid: *const IID,
        ppvObject: *mut *mut c_void,
    ) -> HRESULT {
        if ppvObject.is_null() {
            return winapi::shared::winerror::E_POINTER;
        }
        if riid.is_null() {
            return E_NOINTERFACE;
        }

        let iid = &*riid;
        if is_guid_equal(iid, &IID_I_UNKNOWN) || is_guid_equal(iid, &IID_IMM_NOTIFICATION_CLIENT) {
            *ppvObject = this as *mut c_void;
            AddRef(this);
            S_OK
        } else {
            *ppvObject = null_mut();
            E_NOINTERFACE
        }
    }

    unsafe extern "system" fn AddRef(this: *mut IMMNotificationClient) -> ULONG {
        (*this).ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    unsafe extern "system" fn Release(this: *mut IMMNotificationClient) -> ULONG {
        let prev = (*this).ref_count.fetch_sub(1, Ordering::SeqCst);
        prev.saturating_sub(1)
    }

    unsafe extern "system" fn OnDeviceStateChanged(
        _this: *mut IMMNotificationClient,
        _pwstrDeviceId: LPCWSTR,
        _dwNewState: DWORD,
    ) -> HRESULT {
        trigger_device_reload();
        S_OK
    }

    unsafe extern "system" fn OnDeviceAdded(
        _this: *mut IMMNotificationClient,
        _pwstrDeviceId: LPCWSTR,
    ) -> HRESULT {
        trigger_device_reload();
        S_OK
    }

    unsafe extern "system" fn OnDeviceRemoved(
        _this: *mut IMMNotificationClient,
        _pwstrDeviceId: LPCWSTR,
    ) -> HRESULT {
        trigger_device_reload();
        S_OK
    }

    unsafe extern "system" fn OnDefaultDeviceChanged(
        _this: *mut IMMNotificationClient,
        flow: DWORD,
        _role: DWORD,
        _pwstrDefaultDeviceId: LPCWSTR,
    ) -> HRESULT {
        // flow 0 = eRender (audio output device)
        if flow == 0 {
            trigger_device_reload();
        }
        S_OK
    }

    unsafe extern "system" fn OnPropertyValueChanged(
        _this: *mut IMMNotificationClient,
        _pwstrDeviceId: LPCWSTR,
        _key: *const PROPERTYKEY,
    ) -> HRESULT {
        S_OK
    }

    fn is_guid_equal(a: &GUID, b: &GUID) -> bool {
        a.Data1 == b.Data1 && a.Data2 == b.Data2 && a.Data3 == b.Data3 && a.Data4 == b.Data4
    }

    static VTBL: IMMNotificationClientVtbl = IMMNotificationClientVtbl {
        QueryInterface,
        AddRef,
        Release,
        OnDeviceStateChanged,
        OnDeviceAdded,
        OnDeviceRemoved,
        OnDefaultDeviceChanged,
        OnPropertyValueChanged,
    };

    static mut CLIENT_INSTANCE: IMMNotificationClient = IMMNotificationClient {
        lpVtbl: &VTBL,
        ref_count: AtomicU32::new(1),
    };

    #[repr(C)]
    struct IMMDeviceEnumeratorVtbl {
        QueryInterface: unsafe extern "system" fn(
            this: *mut c_void,
            riid: *const IID,
            ppvObject: *mut *mut c_void,
        ) -> HRESULT,
        AddRef: unsafe extern "system" fn(this: *mut c_void) -> ULONG,
        Release: unsafe extern "system" fn(this: *mut c_void) -> ULONG,
        EnumAudioEndpoints: unsafe extern "system" fn(
            this: *mut c_void,
            dataFlow: DWORD,
            dwStateMask: DWORD,
            ppDevices: *mut *mut c_void,
        ) -> HRESULT,
        GetDefaultAudioEndpoint: unsafe extern "system" fn(
            this: *mut c_void,
            dataFlow: DWORD,
            role: DWORD,
            ppEndpoint: *mut *mut c_void,
        ) -> HRESULT,
        GetDevice: unsafe extern "system" fn(
            this: *mut c_void,
            pwstrId: LPCWSTR,
            ppDevice: *mut *mut c_void,
        ) -> HRESULT,
        RegisterEndpointNotificationCallback: unsafe extern "system" fn(
            this: *mut c_void,
            pClient: *mut IMMNotificationClient,
        ) -> HRESULT,
        UnregisterEndpointNotificationCallback: unsafe extern "system" fn(
            this: *mut c_void,
            pClient: *mut IMMNotificationClient,
        ) -> HRESULT,
    }

    #[repr(C)]
    struct IMMDeviceEnumerator {
        lpVtbl: *const IMMDeviceEnumeratorVtbl,
    }

    pub fn start() {
        std::thread::spawn(|| {
            unsafe {
                let _ = CoInitializeEx(null_mut(), COINIT_MULTITHREADED);

                let mut enumerator_ptr: *mut c_void = null_mut();
                let hr = CoCreateInstance(
                    &CLSID_MM_DEVICE_ENUMERATOR,
                    null_mut(),
                    CLSCTX_INPROC_SERVER,
                    &IID_IMM_DEVICE_ENUMERATOR,
                    &mut enumerator_ptr,
                );

                if hr == S_OK && !enumerator_ptr.is_null() {
                    let enumerator = enumerator_ptr as *mut IMMDeviceEnumerator;
                    let reg_hr = ((*(*enumerator).lpVtbl).RegisterEndpointNotificationCallback)(
                        enumerator as *mut c_void,
                        &raw mut CLIENT_INSTANCE,
                    );
                    if reg_hr == S_OK {
                        crate::always_print!("🔊 [DeviceWatcher] Audio endpoint hotplug & default device listener active");
                    } else {
                        crate::always_eprint!("⚠️ [DeviceWatcher] RegisterEndpointNotificationCallback returned hr=0x{:X}", reg_hr);
                    }
                } else {
                    crate::always_eprint!("⚠️ [DeviceWatcher] CoCreateInstance(MMDeviceEnumerator) failed hr=0x{:X}", hr);
                }
            }
        });
    }
}

pub fn start_device_watcher() {
    #[cfg(target_os = "windows")]
    {
        windows_impl::start();
    }
}

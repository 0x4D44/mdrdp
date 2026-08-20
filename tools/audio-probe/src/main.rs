//! Ask WASAPI whether this host has a render endpoint that loopback capture
//! could attach to — and, if it does, whether a loopback client will actually
//! initialise.
//!
//! Tranche 6 (audio) rides on one assumption: that rhydra can capture what the
//! session is playing. On a desktop that is obviously true. On a **headless**
//! box it is not, and the failure is quiet: WASAPI loopback attaches to a
//! *render* endpoint, so a machine with nothing plugged into its audio jack has
//! nothing to attach to. `Win32_SoundDevice` still cheerfully reports the
//! Realtek adapter, which is why the registry and WMI both look fine while the
//! feature cannot work.
//!
//! This exists so that answer is evidence rather than inference. It reports the
//! state of every render endpoint, whether a default one exists, and whether a
//! loopback `IAudioClient` initialises — the three facts that decide whether the
//! tranche proceeds as designed or needs a virtual audio device on the host.
//!
//! Read-only. It initialises a capture client and immediately drops it; it never
//! starts the stream, so it cannot disturb anything already playing.

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn devnode_property_strings(
    devinst: u32,
    key: &windows::Win32::Foundation::PROPERTYKEY,
) -> Vec<String> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_PropertyW, CR_BUFFER_SMALL, CR_SUCCESS,
    };
    use windows::Win32::Devices::Properties::{
        DEVPROPTYPE, DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST,
    };
    use windows::Win32::Foundation::DEVPROPKEY;

    let key = DEVPROPKEY {
        fmtid: key.fmtid,
        pid: key.pid,
    };
    let mut property_type = DEVPROPTYPE(0);
    let mut bytes = 0u32;
    let status =
        unsafe { CM_Get_DevNode_PropertyW(devinst, &key, &mut property_type, None, &mut bytes, 0) };
    if status != CR_BUFFER_SMALL
        || !matches!(
            property_type,
            DEVPROP_TYPE_STRING | DEVPROP_TYPE_STRING_LIST
        )
        || bytes < 2
    {
        return Vec::new();
    }
    let mut buffer = vec![0u8; bytes as usize];
    let status = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &key,
            &mut property_type,
            Some(buffer.as_mut_ptr()),
            &mut bytes,
            0,
        )
    };
    if status != CR_SUCCESS {
        return Vec::new();
    }
    buffer[..bytes as usize]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>()
        .split(|value| *value == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(windows)]
fn stable_parent_identity(parent: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Status, CM_Locate_DevNodeW, CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS,
        DN_HAS_PROBLEM,
    };
    use windows::Win32::Devices::FunctionDiscovery::{
        PKEY_Device_DriverProvider, PKEY_Device_HardwareIds, PKEY_Device_Manufacturer,
        PKEY_Device_Service,
    };

    if !parent.to_ascii_uppercase().starts_with("ROOT\\MEDIA\\") {
        return None;
    }
    let parent_wide = wide(parent);
    let mut devinst = 0u32;
    if unsafe {
        CM_Locate_DevNodeW(
            &mut devinst,
            PCWSTR::from_raw(parent_wide.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        )
    } != CR_SUCCESS
    {
        return None;
    }
    let mut status =
        windows::Win32::Devices::DeviceAndDriverInstallation::CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = windows::Win32::Devices::DeviceAndDriverInstallation::CM_PROB(0);
    if unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) } != CR_SUCCESS
        || status.contains(DN_HAS_PROBLEM)
    {
        return None;
    }
    let hardware = devnode_property_strings(devinst, &PKEY_Device_HardwareIds);
    let service = devnode_property_strings(devinst, &PKEY_Device_Service);
    let manufacturer = devnode_property_strings(devinst, &PKEY_Device_Manufacturer);
    let provider = devnode_property_strings(devinst, &PKEY_Device_DriverProvider);
    let exact = hardware
        .iter()
        .any(|value| value.eq_ignore_ascii_case("VBAudioVACWDM"))
        && service
            .iter()
            .any(|value| value.eq_ignore_ascii_case("VBAudioVACMME"))
        && manufacturer
            .iter()
            .any(|value| value.eq_ignore_ascii_case("VB-Audio Software"))
        && provider
            .iter()
            .any(|value| value.eq_ignore_ascii_case("VB-Audio Software"));
    exact.then(|| {
        format!(
            "parent={parent}, hardware=VBAudioVACWDM, service=VBAudioVACMME, provider=VB-Audio Software"
        )
    })
}

#[cfg(windows)]
fn software_device_facts(
    device: &windows::Win32::Media::Audio::IMMDevice,
) -> Option<(String, String)> {
    use windows::core::PCWSTR;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Locate_DevNodeW, CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS,
    };
    use windows::Win32::Devices::FunctionDiscovery::{
        PKEY_Device_BusReportedDeviceDesc, PKEY_Device_Parent,
    };
    use windows::Win32::System::Com::CoTaskMemFree;

    let id = unsafe { device.GetId() }.ok()?;
    let endpoint_id = unsafe {
        let mut len = 0usize;
        while *id.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(id.0, len))
    };
    unsafe { CoTaskMemFree(Some(id.0.cast())) };
    let instance = wide(&format!(r"SWD\MMDEVAPI\{endpoint_id}"));
    let mut devinst = 0u32;
    if unsafe {
        CM_Locate_DevNodeW(
            &mut devinst,
            PCWSTR::from_raw(instance.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        )
    } != CR_SUCCESS
    {
        return None;
    }
    let parent = devnode_property_strings(devinst, &PKEY_Device_Parent)
        .into_iter()
        .next()?;
    let bus = devnode_property_strings(devinst, &PKEY_Device_BusReportedDeviceDesc)
        .into_iter()
        .next()?;
    Some((parent, bus))
}

#[cfg(windows)]
fn property_string(
    device: &windows::Win32::Media::Audio::IMMDevice,
    key: &windows::Win32::Foundation::PROPERTYKEY,
) -> Option<String> {
    use windows::Win32::System::Com::StructuredStorage::PropVariantToString;
    use windows::Win32::System::Com::STGM_READ;

    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let value = unsafe { store.GetValue(key) }.ok()?;
    let mut buffer = [0u16; 512];
    unsafe { PropVariantToString(&value, &mut buffer) }.ok()?;
    let end = buffer
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

#[cfg(windows)]
fn format_text(device: &windows::Win32::Media::Audio::IMMDevice) -> Option<String> {
    use windows::Win32::Media::Audio::IAudioClient;
    use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL};

    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.ok()?;
    let format = unsafe { client.GetMixFormat() }.ok()?;
    if format.is_null() {
        return None;
    }
    let text = unsafe {
        let rate = std::ptr::addr_of!((*format).nSamplesPerSec).read_unaligned();
        let channels = std::ptr::addr_of!((*format).nChannels).read_unaligned();
        let bits = std::ptr::addr_of!((*format).wBitsPerSample).read_unaligned();
        format!("{} Hz, {} ch, {}-bit", rate, channels, bits)
    };
    unsafe { CoTaskMemFree(Some(format as *const _)) };
    Some(text)
}

#[cfg(windows)]
fn print_active_endpoint_details(
    enumerator: &windows::Win32::Media::Audio::IMMDeviceEnumerator,
    flow: windows::Win32::Media::Audio::EDataFlow,
    label: &str,
) {
    use windows::Win32::Devices::FunctionDiscovery::{
        PKEY_Device_BusReportedDeviceDesc, PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName,
        PKEY_Device_MatchingDeviceId, PKEY_Device_Parent,
    };
    use windows::Win32::Media::Audio::DEVICE_STATE_ACTIVE;
    use windows::Win32::Media::Audio::{PKEY_AudioEndpoint_FormFactor, PKEY_AudioEndpoint_GUID};

    let Ok(collection) = (unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) })
    else {
        println!("audio-probe: {label} endpoint details unavailable");
        return;
    };
    let count = unsafe { collection.GetCount() }.unwrap_or(0);
    for index in 0..count {
        let Ok(device) = (unsafe { collection.Item(index) }) else {
            continue;
        };
        let friendly = property_string(&device, &PKEY_Device_FriendlyName)
            .unwrap_or_else(|| "<unnamed>".to_owned());
        let description = property_string(&device, &PKEY_Device_DeviceDesc)
            .unwrap_or_else(|| "<no PnP description>".to_owned());
        let software_facts = software_device_facts(&device);
        let bus_description = software_facts
            .as_ref()
            .map(|(_, bus)| bus.clone())
            .or_else(|| property_string(&device, &PKEY_Device_BusReportedDeviceDesc))
            .unwrap_or_else(|| "<no bus role>".to_owned());
        let matching = property_string(&device, &PKEY_Device_MatchingDeviceId)
            .unwrap_or_else(|| "<no matching ID>".to_owned());
        let parent = software_facts
            .map(|(parent, _)| parent)
            .or_else(|| property_string(&device, &PKEY_Device_Parent))
            .unwrap_or_else(|| "<no parent>".to_owned());
        let form_factor = property_string(&device, &PKEY_AudioEndpoint_FormFactor)
            .unwrap_or_else(|| "<unknown>".to_owned());
        let endpoint_guid = property_string(&device, &PKEY_AudioEndpoint_GUID)
            .unwrap_or_else(|| "<unknown>".to_owned());
        let format = format_text(&device).unwrap_or_else(|| "<unavailable>".to_owned());
        let stable_identity =
            stable_parent_identity(&parent).unwrap_or_else(|| "not VB-CABLE".to_owned());
        println!(
            "  {label} endpoint {index}: {friendly:?}; bus_role={bus_description:?}; description={description:?}; format={format}; matching={matching:?}; stable_identity={stable_identity}; form_factor={form_factor}; endpoint_guid={endpoint_guid}"
        );
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE, DEVICE_STATE_ACTIVE,
        DEVICE_STATE_DISABLED, DEVICE_STATE_NOTPRESENT, DEVICE_STATE_UNPLUGGED,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    // SAFETY: called once, before any other COM call on this thread.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    // SAFETY: standard COM activation of the endpoint enumerator.
    let enumerator: IMMDeviceEnumerator =
        match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
            Ok(e) => e,
            Err(e) => {
                println!("audio-probe: cannot create the device enumerator: {e}");
                return std::process::ExitCode::from(2);
            }
        };

    let all = DEVICE_STATE_ACTIVE.0
        | DEVICE_STATE_DISABLED.0
        | DEVICE_STATE_NOTPRESENT.0
        | DEVICE_STATE_UNPLUGGED.0;

    // SAFETY: a valid enumerator and a well-formed state mask.
    let collection = match unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE(all)) } {
        Ok(c) => c,
        Err(e) => {
            println!("audio-probe: cannot enumerate render endpoints: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    // SAFETY: a valid collection.
    let count = unsafe { collection.GetCount() }.unwrap_or(0);
    println!("audio-probe: {count} render endpoint(s)");

    let mut active = 0u32;
    for i in 0..count {
        // SAFETY: i < count, checked by the loop bound.
        let Ok(device) = (unsafe { collection.Item(i) }) else {
            continue;
        };
        // SAFETY: a valid device.
        let state = unsafe { device.GetState() }.unwrap_or_default();
        let name = match state {
            s if s == DEVICE_STATE_ACTIVE => "ACTIVE",
            s if s == DEVICE_STATE_DISABLED => "disabled",
            s if s == DEVICE_STATE_NOTPRESENT => "not-present",
            s if s == DEVICE_STATE_UNPLUGGED => "unplugged",
            _ => "unknown",
        };
        if state == DEVICE_STATE_ACTIVE {
            active += 1;
        }
        println!("  endpoint {i}: {name} (0x{:08x})", state.0);
    }
    println!("audio-probe: {active} ACTIVE");

    // Names and mix formats are evidence for the operator, not discovery
    // keys.  VB-CABLE names vary by package and user customisation, while the
    // server uses the PnP identity to choose its endpoint.
    print_active_endpoint_details(&enumerator, eRender, "render");
    print_active_endpoint_details(&enumerator, eCapture, "capture");

    // The decisive question. Loopback capture attaches to the default render
    // endpoint; if there is no default, the feature has nothing to capture.
    // SAFETY: a valid enumerator.
    let default = match unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) } {
        Ok(d) => {
            let name = property_string(
                &d,
                &windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
            )
            .unwrap_or_else(|| "<unnamed>".to_owned());
            println!("audio-probe: default render endpoint: PRESENT ({name})");
            d
        }
        Err(e) => {
            println!("audio-probe: default render endpoint: NONE ({e})");
            println!(
                "audio-probe: VERDICT — loopback capture cannot work on this host as configured."
            );
            return std::process::ExitCode::from(1);
        }
    };

    // A default endpoint can still refuse a loopback client. Ask.
    // SAFETY: a valid device; Activate with no activation parameters.
    let client: IAudioClient = match unsafe { default.Activate(CLSCTX_ALL, None) } {
        Ok(c) => c,
        Err(e) => {
            println!("audio-probe: cannot activate IAudioClient: {e}");
            return std::process::ExitCode::from(1);
        }
    };
    // SAFETY: a freshly activated client.
    let format = match unsafe { client.GetMixFormat() } {
        Ok(f) => f,
        Err(e) => {
            println!("audio-probe: cannot read the mix format: {e}");
            return std::process::ExitCode::from(1);
        }
    };
    // SAFETY: GetMixFormat returned a valid pointer.
    let (rate, channels, bits) = unsafe {
        (
            (*format).nSamplesPerSec,
            (*format).nChannels,
            (*format).wBitsPerSample,
        )
    };
    println!("audio-probe: mix format {rate} Hz, {channels} ch, {bits}-bit");

    // SAFETY: a valid client and the mix format it just handed us. A 200 ms
    // buffer, shared mode, loopback. Initialised and dropped without Start().
    let init = unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            2_000_000,
            0,
            format,
            None,
        )
    };
    match init {
        Ok(()) => {
            // SAFETY: GetMixFormat returned a CoTaskMem buffer, which is no
            // longer needed after Initialize copied the format.
            unsafe {
                windows::Win32::System::Com::CoTaskMemFree(Some(format as *const _));
            }
            println!("audio-probe: loopback client initialised");
            println!("audio-probe: VERDICT — loopback capture is viable on this host.");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            // SAFETY: GetMixFormat returned a CoTaskMem buffer.
            unsafe {
                windows::Win32::System::Com::CoTaskMemFree(Some(format as *const _));
            }
            println!("audio-probe: loopback initialise failed: {e}");
            println!(
                "audio-probe: VERDICT — loopback capture cannot work on this host as configured."
            );
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("audio-probe: there is no WASAPI on this platform");
    std::process::ExitCode::from(2)
}

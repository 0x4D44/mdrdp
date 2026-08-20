//! Windows Core Audio policy for the base VB-CABLE device.
//!
//! VB-CABLE publishes more than one endpoint on current Pack45 installs.  The
//! user-facing name is also mutable, so discovery keys the device to the PnP
//! parent identity, then selects the driver-reported bus role.
//! Any missing identity or duplicate role fails closed.

use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::ptr;

use serde::{Deserialize, Serialize};
use windows::core::{IUnknown, Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_DevNode_Status, CM_Locate_DevNodeW, CM_LOCATE_DEVNODE_NORMAL,
    CR_BUFFER_SMALL, CR_SUCCESS, DN_HAS_PROBLEM,
};
use windows::Win32::Devices::FunctionDiscovery::{
    PKEY_Device_BusReportedDeviceDesc, PKEY_Device_DriverInfSection, PKEY_Device_DriverProvider,
    PKEY_Device_FriendlyName, PKEY_Device_HardwareIds, PKEY_Device_Manufacturer,
    PKEY_Device_MatchingDeviceId, PKEY_Device_Parent, PKEY_Device_Service,
};
use windows::Win32::Devices::Properties::{
    DEVPROPTYPE, DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST,
};
use windows::Win32::Foundation::{
    CloseHandle, DEVPROPKEY, ERROR_NOT_FOUND, HANDLE, PROPERTYKEY, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::Media::Audio::ERole as AudioRole;
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eMultimedia, eRender, EDataFlow, ERole, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::KernelStreaming::{
    KSDATAFORMAT_SUBTYPE_PCM, SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE,
};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToString;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use super::audio::ensure_com;

/// The only endpoint format this policy writes.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const BITS_PER_SAMPLE: u16 = 16;

const VB_HARDWARE_ID: &str = "VBAudioVACWDM";
const VB_SERVICE: &str = "VBAudioVACMME";
const VB_MANUFACTURER: &str = "VB-Audio Software";
const BASE_RENDER_BUS_DESCRIPTION: &str = "Speakers (VB-Audio Virtual Cable)";
const BASE_CAPTURE_BUS_DESCRIPTION: &str = "CABLE Output (VB-Audio Virtual Cable)";
const CAPTURE_MUTEX: &str = "Local\\mdrdp-audio-capture";
const LEASE_FILE: &str = "audio-lease.json";
const FORMAT_MARKER: &str = r"C:\mdrdp\vb-cable-format.ok";

// IPolicyConfig is not part of the Windows metadata used by windows-rs.  Its
// ABI is stable on supported desktop Windows releases; retain the complete
// prefix so SetDeviceFormat and SetDefaultEndpoint land at the right slots.
//
// This wrapper deliberately uses IUnknown plus a private vtable instead of
// windows-core's define_interface macro.  That macro expands through the
// transitive `windows_core` crate name, which this standalone crate must not
// add as a new direct dependency.
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

#[repr(C)]
#[doc(hidden)]
#[allow(non_snake_case)]
struct IPolicyConfigVtbl {
    base__: windows::core::IUnknown_Vtbl,
    GetMixFormat: usize,
    GetDeviceFormat:
        unsafe extern "system" fn(*mut c_void, PCWSTR, i32, *mut *mut WAVEFORMATEX) -> HRESULT,
    ResetDeviceFormat: usize,
    SetDeviceFormat: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        *mut WAVEFORMATEX,
        *mut WAVEFORMATEX,
    ) -> HRESULT,
    GetProcessingPeriod: usize,
    SetProcessingPeriod: usize,
    GetShareMode: usize,
    SetShareMode: usize,
    GetPropertyValue: usize,
    SetPropertyValue: usize,
    SetDefaultEndpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT,
    SetEndpointVisibility: usize,
}

struct IPolicyConfig {
    inner: IUnknown,
}

impl IPolicyConfig {
    unsafe fn vtable(&self) -> &IPolicyConfigVtbl {
        &**(self.inner.as_raw() as *mut *mut IPolicyConfigVtbl)
    }

    unsafe fn set_device_format(
        &self,
        endpoint_id: PCWSTR,
        format: *mut WAVEFORMATEX,
        default_format: *mut WAVEFORMATEX,
    ) -> windows::core::Result<()> {
        (self.vtable().SetDeviceFormat)(self.inner.as_raw(), endpoint_id, format, default_format)
            .ok()
    }

    unsafe fn get_device_format(
        &self,
        endpoint_id: PCWSTR,
    ) -> windows::core::Result<*mut WAVEFORMATEX> {
        let mut format = ptr::null_mut();
        (self.vtable().GetDeviceFormat)(self.inner.as_raw(), endpoint_id, 0, &mut format).ok()?;
        Ok(format)
    }

    unsafe fn set_default_endpoint(
        &self,
        endpoint_id: PCWSTR,
        role: ERole,
    ) -> windows::core::Result<()> {
        (self.vtable().SetDefaultEndpoint)(self.inner.as_raw(), endpoint_id, role).ok()
    }
}

const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// A discovered endpoint, including names only for evidence.  Selection never
/// uses `friendly_name` because a user can rename an endpoint at any time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    pub id: String,
    pub bus_description: String,
    pub friendly_name: String,
    pub parent: String,
    pub matching_device_id: String,
    pub inf_section: String,
    pub driver_provider: String,
}

/// The base CABLE render endpoint and its loopback capture partner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CableEndpoints {
    pub render: EndpointInfo,
    pub capture: EndpointInfo,
}

/// The endpoint returned to loopback capture.  The lease and mutex live as long
/// as the source, so a crash leaves the durable record for the next agent start.
pub struct CaptureTarget {
    pub render_id: String,
    _lease: DefaultLease,
    _capture_lock: CaptureLock,
}

/// Discover the one base CABLE render/capture pair.
pub fn discover_cable() -> Result<CableEndpoints, String> {
    ensure_com();
    let enumerator = enumerator()?;
    let render = enumerate(&enumerator, eRender)?;
    let capture = enumerate(&enumerator, eCapture)?;
    let render = choose_role(render, true)?;
    let capture = choose_role(capture, false)?;
    Ok(CableEndpoints { render, capture })
}

/// Pin both endpoints to 48 kHz PCM16 stereo. Existing render defaults are left
/// alone; a role with no default is set to CABLE so the health ladder can start.
/// Capture still owns the temporary lease for any displaced defaults.
pub fn configure_audio() -> Result<String, String> {
    let _capture_lock = CaptureLock::try_acquire()?;
    let cable = discover_cable()?;
    pin_formats(&cable)?;
    let current = default_ids()?;
    set_missing_defaults(&current, &cable.render.id, set_default)?;
    Ok(format!(
        "VB-CABLE configured: render {:?}, capture {:?}, {} Hz PCM16 stereo",
        cable.render.friendly_name, cable.capture.friendly_name, SAMPLE_RATE
    ))
}

/// Read back the live endpoint policy without mutating it. Deploy uses this to
/// reject a stale success marker after a user or driver changes either format.
pub fn check_audio() -> Result<String, String> {
    let cable = discover_cable()?;
    verify_formats(&cable)?;
    if !all_defaults_present(&default_ids()?) {
        return Err("one or more Windows render roles have no default endpoint".to_owned());
    }
    Ok(format!(
        "VB-CABLE verified: render {:?}, capture {:?}, {} Hz PCM16 stereo",
        cable.render.friendly_name, cable.capture.friendly_name, SAMPLE_RATE
    ))
}

/// Persist the deploy preflight marker from the elevated SSH-side wrapper,
/// after the interactive agent has read back both endpoint formats.
pub fn write_format_marker(evidence: &str) -> Result<(), String> {
    let marker = PathBuf::from(FORMAT_MARKER);
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create format marker directory {}: {error}",
                parent.display()
            )
        })?;
    }
    fs::write(
        &marker,
        format!("48kHz PCM16 stereo verified\n{evidence}\n"),
    )
    .map_err(|error| format!("write verified format marker {}: {error}", marker.display()))?;
    Ok(())
}

/// Acquire the interactive capture lease and return the exact render endpoint.
pub fn begin_capture() -> Result<CaptureTarget, String> {
    let capture_lock = CaptureLock::try_acquire()?;
    recover_stale_lease_locked()?;
    let cable = discover_cable()?;
    pin_formats(&cable)?;
    let lease = DefaultLease::install(&cable.render.id)?;
    Ok(CaptureTarget {
        render_id: cable.render.id,
        _lease: lease,
        _capture_lock: capture_lock,
    })
}

/// Recover a lease left by a terminated server before supervision starts it.
pub fn recover_stale_lease() -> Result<(), String> {
    let _capture_lock = CaptureLock::try_acquire()?;
    recover_stale_lease_locked()
}

fn recover_stale_lease_locked() -> Result<(), String> {
    let path = lease_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let record: LeaseRecord = serde_json::from_str(&text)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    restore_record(&record)?;
    fs::remove_file(&path).map_err(|error| format!("remove {}: {error}", path.display()))
}

fn pin_formats(cable: &CableEndpoints) -> Result<(), String> {
    let policy = policy_config()?;
    for endpoint in [&cable.render, &cable.capture] {
        let mut format = target_format();
        let mut default_format = WAVEFORMATEXTENSIBLE::default();
        let id = wide(&endpoint.id);
        // SAFETY: `id` and `format` stay alive for the synchronous COM call;
        // IPolicyConfig does not retain either pointer.
        unsafe {
            policy.set_device_format(
                PCWSTR::from_raw(id.as_ptr()),
                ptr::addr_of_mut!(format).cast(),
                ptr::addr_of_mut!(default_format).cast(),
            )
        }
        .map_err(|error| format!("pin {} to 48 kHz PCM16 stereo: {error}", endpoint.id))?;
    }
    verify_formats_with_policy(cable, &policy)
}

fn verify_formats(cable: &CableEndpoints) -> Result<(), String> {
    let policy = policy_config()?;
    verify_formats_with_policy(cable, &policy)
}

fn verify_formats_with_policy(
    cable: &CableEndpoints,
    policy: &IPolicyConfig,
) -> Result<(), String> {
    for endpoint in [&cable.render, &cable.capture] {
        let id = wide(&endpoint.id);
        // A successful SetDeviceFormat HRESULT and a deploy marker are not
        // evidence until the policy store returns the exact live format.
        let actual = unsafe { policy.get_device_format(PCWSTR::from_raw(id.as_ptr())) }
            .map_err(|error| format!("read back format for {}: {error}", endpoint.id))?;
        if actual.is_null() {
            return Err(format!(
                "read back format for {} returned null",
                endpoint.id
            ));
        }
        let matches = unsafe { format_matches(actual) };
        unsafe { CoTaskMemFree(Some(actual.cast())) };
        if !matches {
            return Err(format!(
                "format verification failed for {}; expected 48 kHz PCM16 stereo",
                endpoint.id
            ));
        }
    }
    Ok(())
}

fn target_format() -> WAVEFORMATEXTENSIBLE {
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: CHANNELS,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8),
            nBlockAlign: CHANNELS * (BITS_PER_SAMPLE / 8),
            wBitsPerSample: BITS_PER_SAMPLE,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
                - std::mem::size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: BITS_PER_SAMPLE,
        },
        dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
    }
}

unsafe fn format_matches(actual: *const WAVEFORMATEX) -> bool {
    let base = unsafe { ptr::read_unaligned(actual) };
    if base.wFormatTag != WAVE_FORMAT_EXTENSIBLE as u16
        || base.nChannels != CHANNELS
        || base.nSamplesPerSec != SAMPLE_RATE
        || base.wBitsPerSample != BITS_PER_SAMPLE
        || usize::from(base.cbSize)
            < std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()
    {
        return false;
    }
    let extended = unsafe { ptr::read_unaligned(actual.cast::<WAVEFORMATEXTENSIBLE>()) };
    let valid_bits = unsafe { extended.Samples.wValidBitsPerSample };
    let subformat = unsafe { ptr::addr_of!(extended.SubFormat).read_unaligned() };
    valid_bits == BITS_PER_SAMPLE
        && extended.dwChannelMask == SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
        && subformat == KSDATAFORMAT_SUBTYPE_PCM
}

fn enumerate(
    enumerator: &IMMDeviceEnumerator,
    flow: EDataFlow,
) -> Result<Vec<EndpointInfo>, String> {
    let collection = unsafe {
        enumerator.EnumAudioEndpoints(flow, windows::Win32::Media::Audio::DEVICE_STATE_ACTIVE)
    }
    .map_err(|error| format!("enumerate audio endpoints: {error}"))?;
    let count = unsafe { collection.GetCount() }
        .map_err(|error| format!("count audio endpoints: {error}"))?;
    let mut result = Vec::with_capacity(count as usize);
    for index in 0..count {
        let device = unsafe { collection.Item(index) }
            .map_err(|error| format!("read audio endpoint {index}: {error}"))?;
        if let Some(info) = endpoint_info(&device)? {
            result.push(info);
        }
    }
    Ok(result)
}

fn choose_role(candidates: Vec<EndpointInfo>, render: bool) -> Result<EndpointInfo, String> {
    let expected = if render {
        BASE_RENDER_BUS_DESCRIPTION
    } else {
        BASE_CAPTURE_BUS_DESCRIPTION
    };
    let selected: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.bus_description.eq_ignore_ascii_case(expected))
        .collect();
    match selected.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "VB-CABLE {} endpoint not found after PnP identity filtering",
            if render { "render" } else { "capture" }
        )),
        many => Err(format!(
            "ambiguous VB-CABLE {} endpoints ({} matches); refusing to guess",
            if render { "render" } else { "capture" },
            many.len()
        )),
    }
}

fn endpoint_info(device: &IMMDevice) -> Result<Option<EndpointInfo>, String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }
        .map_err(|error| format!("open audio endpoint properties: {error}"))?;
    let matching = property_string(&store, &PKEY_Device_MatchingDeviceId)?;
    let inf_section = property_string(&store, &PKEY_Device_DriverInfSection)?;
    let Some(parent) = property_string(&store, &PKEY_Device_Parent)? else {
        return Ok(None);
    };
    if !parent_identity_matches(&parent) {
        return Ok(None);
    }
    let id = device_id(device)?;
    Ok(Some(EndpointInfo {
        id,
        bus_description: property_string(&store, &PKEY_Device_BusReportedDeviceDesc)?
            .ok_or("VB-CABLE endpoint has no bus-reported role description")?,
        friendly_name: property_string(&store, &PKEY_Device_FriendlyName)?
            .unwrap_or_else(|| "<unnamed>".to_owned()),
        parent,
        // MMDevAPI commonly reports its wrapper identity as Microsoft. Keep
        // these endpoint values in evidence, but select only by the resolved
        // parent PnP identity below.
        matching_device_id: matching.unwrap_or_default(),
        inf_section: inf_section.unwrap_or_default(),
        driver_provider: property_string(&store, &PKEY_Device_DriverProvider)?.unwrap_or_default(),
    }))
}

/// Resolve the endpoint's parent devnode and require the vendor identity on
/// that devnode.  The MMDevice wrapper itself reports Microsoft and generic
/// `MMDEVAPI` hardware IDs, so endpoint properties alone are not a stable
/// way to identify VB-CABLE.
fn parent_identity_matches(parent: &str) -> bool {
    if !parent.to_ascii_uppercase().starts_with("ROOT\\MEDIA\\") {
        return false;
    }
    let parent_wide = wide(parent);
    let mut devinst = 0u32;
    let located = unsafe {
        CM_Locate_DevNodeW(
            &mut devinst,
            PCWSTR::from_raw(parent_wide.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        )
    };
    if located != CR_SUCCESS {
        return false;
    }

    let mut status =
        windows::Win32::Devices::DeviceAndDriverInstallation::CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = windows::Win32::Devices::DeviceAndDriverInstallation::CM_PROB(0);
    let status_result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) };
    if status_result != CR_SUCCESS || status.contains(DN_HAS_PROBLEM) {
        return false;
    }

    let hardware_ids = devnode_property_strings(devinst, &PKEY_Device_HardwareIds);
    let service = devnode_property_strings(devinst, &PKEY_Device_Service);
    let manufacturer = devnode_property_strings(devinst, &PKEY_Device_Manufacturer);
    let provider = devnode_property_strings(devinst, &PKEY_Device_DriverProvider);
    hardware_ids
        .iter()
        .any(|value| value.eq_ignore_ascii_case(VB_HARDWARE_ID))
        && service
            .iter()
            .any(|value| value.eq_ignore_ascii_case(VB_SERVICE))
        && manufacturer
            .iter()
            .any(|value| value.eq_ignore_ascii_case(VB_MANUFACTURER))
        && provider
            .iter()
            .any(|value| value.eq_ignore_ascii_case(VB_MANUFACTURER))
}

fn devnode_property_strings(devinst: u32, key: &PROPERTYKEY) -> Vec<String> {
    let key = DEVPROPKEY {
        fmtid: key.fmtid,
        pid: key.pid,
    };
    let mut property_type = DEVPROPTYPE(0);
    let mut byte_count = 0u32;
    let result = unsafe {
        CM_Get_DevNode_PropertyW(devinst, &key, &mut property_type, None, &mut byte_count, 0)
    };
    if result != CR_BUFFER_SMALL
        || !matches!(
            property_type,
            DEVPROP_TYPE_STRING | DEVPROP_TYPE_STRING_LIST
        )
        || byte_count < 2
    {
        return Vec::new();
    }
    let mut bytes = vec![0u8; byte_count as usize];
    let result = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &key,
            &mut property_type,
            Some(bytes.as_mut_ptr()),
            &mut byte_count,
            0,
        )
    };
    if result != CR_SUCCESS {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut current = String::new();
    for pair in bytes[..byte_count as usize].chunks_exact(2) {
        let value = u16::from_le_bytes([pair[0], pair[1]]);
        if value == 0 {
            if current.is_empty() {
                break;
            }
            result.push(std::mem::take(&mut current));
        } else {
            current.push(char::from_u32(u32::from(value)).unwrap_or('\u{fffd}'));
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn property_string(
    store: &IPropertyStore,
    key: &windows::Win32::Foundation::PROPERTYKEY,
) -> Result<Option<String>, String> {
    let value = unsafe { store.GetValue(key) };
    let value = match value {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let mut buffer = [0u16; 512];
    // SAFETY: `value` is an owned PROPVARIANT and `buffer` is writable storage
    // for the synchronous propsys conversion.
    unsafe { PropVariantToString(&value, &mut buffer) }
        .map_err(|error| format!("convert endpoint property to text: {error}"))?;
    let end = buffer
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(buffer.len());
    Ok(Some(String::from_utf16_lossy(&buffer[..end])))
}

fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|error| format!("create audio device enumerator: {error}"))
}

fn policy_config() -> Result<IPolicyConfig, String> {
    let unknown: IUnknown = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }
        .map_err(|error| format!("create Core Audio policy client: {error}"))?;
    let mut raw = ptr::null_mut();
    // SAFETY: `unknown` is a live COM object and `raw` points to storage for
    // the queried interface pointer.  The IID is the documented IPolicyConfig
    // identity used by the policy client implementation.
    unsafe {
        (unknown.vtable().QueryInterface)(unknown.as_raw(), &IID_POLICY_CONFIG, &mut raw)
            .ok()
            .map_err(|error| format!("query IPolicyConfig: {error}"))?;
        if raw.is_null() {
            return Err("query IPolicyConfig returned a null interface".to_owned());
        }
        Ok(IPolicyConfig {
            inner: IUnknown::from_raw(raw),
        })
    }
}

fn device_id(device: &IMMDevice) -> Result<String, String> {
    let id =
        unsafe { device.GetId() }.map_err(|error| format!("read audio endpoint ID: {error}"))?;
    let value = unsafe {
        let mut len = 0usize;
        while *id.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(id.0, len))
    };
    // SAFETY: IMMDevice::GetId allocates with the COM task allocator.
    unsafe { CoTaskMemFree(Some(id.0.cast())) };
    Ok(value)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn default_ids() -> Result<[Option<String>; 3], String> {
    let enumerator = enumerator()?;
    let mut ids = Vec::with_capacity(3);
    for role in [eConsole, eMultimedia, eCommunications] {
        match unsafe { enumerator.GetDefaultAudioEndpoint(eRender, role) } {
            Ok(device) => ids.push(Some(device_id(&device)?)),
            Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => ids.push(None),
            Err(error) => {
                return Err(format!(
                    "read default render endpoint for role {role:?}: {error}"
                ));
            }
        }
    }
    ids.try_into()
        .map_err(|_| "Core Audio returned the wrong number of default roles".to_owned())
}

fn set_default(id: &str, role: AudioRole) -> Result<(), String> {
    let policy = policy_config()?;
    let id = wide(id);
    // SAFETY: the UTF-16 ID is NUL-terminated and lives for the COM call.
    unsafe { policy.set_default_endpoint(PCWSTR::from_raw(id.as_ptr()), role) }
        .map_err(|error| format!("set default endpoint for role {role:?}: {error}"))
}

fn set_missing_defaults(
    current: &[Option<String>; 3],
    cable_render_id: &str,
    mut set: impl FnMut(&str, AudioRole) -> Result<(), String>,
) -> Result<(), String> {
    let roles = [eConsole, eMultimedia, eCommunications];
    for (index, role) in roles.into_iter().enumerate() {
        if current[index].is_none() {
            set(cable_render_id, role)?;
        }
    }
    Ok(())
}

fn all_defaults_present(current: &[Option<String>; 3]) -> bool {
    current.iter().all(Option::is_some)
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseRecord {
    cable_render_id: String,
    displaced: [Option<String>; 3],
}

struct DefaultLease {
    path: PathBuf,
    record: Option<LeaseRecord>,
}

impl DefaultLease {
    fn install(cable_render_id: &str) -> Result<Self, String> {
        let previous = default_ids()?;
        if previous
            .iter()
            .all(|id| id.as_deref() == Some(cable_render_id))
        {
            return Ok(Self {
                path: lease_path()?,
                record: None,
            });
        }
        let path = lease_path()?;
        let record = LeaseRecord {
            cable_render_id: cable_render_id.to_owned(),
            displaced: previous,
        };
        write_lease(&path, &record)?;
        if let Err(error) = apply_default_transaction(&record, set_default) {
            // Keep the durable record even after a successful rollback. It is
            // the only way a later agent can finish a role that previously had
            // no default (there is no supported "clear default" policy call).
            return Err(error);
        }
        Ok(Self {
            path,
            record: Some(record),
        })
    }
}

impl Drop for DefaultLease {
    fn drop(&mut self) {
        let Some(record) = self.record.as_ref() else {
            return;
        };
        match restore_record(record) {
            Ok(()) => {
                let _ = fs::remove_file(&self.path);
            }
            Err(error) => eprintln!("audio: default endpoint lease retained for recovery: {error}"),
        }
    }
}

fn restore_record(record: &LeaseRecord) -> Result<(), String> {
    let current = default_ids()?;
    restore_owned_defaults(&current, record, set_default)
}

fn apply_default_transaction(
    record: &LeaseRecord,
    mut set: impl FnMut(&str, AudioRole) -> Result<(), String>,
) -> Result<(), String> {
    let roles = [eConsole, eMultimedia, eCommunications];
    let mut changed: Vec<(usize, AudioRole)> = Vec::new();
    for (index, role) in roles.into_iter().enumerate() {
        if record.displaced[index].as_deref() == Some(record.cable_render_id.as_str()) {
            continue;
        }
        if let Err(error) = set(&record.cable_render_id, role) {
            let mut rollback_error = None;
            for &(rollback_index, rollback_role) in changed.iter().rev() {
                // A role that previously had no endpoint deliberately remains
                // CABLE: IPolicyConfig has no supported "clear default" call.
                if let Some(displaced) = record.displaced[rollback_index].as_deref() {
                    if let Err(error) = set(displaced, rollback_role) {
                        rollback_error.get_or_insert(error);
                    }
                }
            }
            return Err(match rollback_error {
                Some(rollback_error) => format!(
                    "default-role transaction failed: {error}; rollback failed: {rollback_error}"
                ),
                None => format!("default-role transaction failed: {error}"),
            });
        }
        changed.push((index, role));
    }
    Ok(())
}

fn restore_owned_defaults(
    current: &[Option<String>; 3],
    record: &LeaseRecord,
    mut set: impl FnMut(&str, AudioRole) -> Result<(), String>,
) -> Result<(), String> {
    let roles = [eConsole, eMultimedia, eCommunications];
    let mut first_error = None;
    for (index, role) in roles.into_iter().enumerate() {
        if current[index].as_deref() != Some(record.cable_render_id.as_str()) {
            // The user or the OS already selected another endpoint. Preserve it.
            continue;
        }
        if let Some(displaced) = record.displaced[index].as_deref() {
            if let Err(error) = set(displaced, role) {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn write_lease(path: &PathBuf, record: &LeaseRecord) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("lease path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let text =
        serde_json::to_vec(record).map_err(|error| format!("encode audio lease: {error}"))?;
    if let Err(error) = fs::write(&temp, text) {
        let _ = fs::remove_file(&temp);
        return Err(format!("write {}: {error}", temp.display()));
    }
    let from = wide(&temp.to_string_lossy());
    let to = wide(&path.to_string_lossy());
    // SAFETY: both paths are NUL-terminated and valid for the synchronous call.
    let moved = unsafe {
        MoveFileExW(
            PCWSTR::from_raw(from.as_ptr()),
            PCWSTR::from_raw(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if let Err(error) = moved {
        let _ = fs::remove_file(&temp);
        return Err(format!("atomically publish {}: {error}", path.display()));
    }
    Ok(())
}

fn lease_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or("LOCALAPPDATA is unavailable; cannot persist audio lease")?;
    Ok(PathBuf::from(base).join("mdrdp").join(LEASE_FILE))
}

struct CaptureLock {
    handle: HANDLE,
}

impl CaptureLock {
    fn try_acquire() -> Result<Self, String> {
        let name = wide(CAPTURE_MUTEX);
        let handle = unsafe { CreateMutexW(None, false, PCWSTR::from_raw(name.as_ptr())) }
            .map_err(|error| format!("create audio capture guard: {error}"))?;
        let wait = unsafe { WaitForSingleObject(handle, 0) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Ok(Self { handle })
        } else {
            let _ = unsafe { CloseHandle(handle) };
            Err("audio capture is active; configure-audio refused".to_owned())
        }
    }
}

impl Drop for CaptureLock {
    fn drop(&mut self) {
        // SAFETY: this handle is owned and acquired by this guard.
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(displaced: [Option<&str>; 3]) -> LeaseRecord {
        LeaseRecord {
            cable_render_id: "cable".to_owned(),
            displaced: displaced.map(|id| id.map(str::to_owned)),
        }
    }

    #[test]
    fn format_contract_is_extensible_pcm16_stereo() {
        let format = target_format();
        let base = unsafe { ptr::addr_of!(format.Format).read_unaligned() };
        let valid = unsafe { format.Samples.wValidBitsPerSample };
        let mask = unsafe { ptr::addr_of!(format.dwChannelMask).read_unaligned() };
        let subtype = unsafe { ptr::addr_of!(format.SubFormat).read_unaligned() };
        let tag = base.wFormatTag;
        let rate = base.nSamplesPerSec;
        let channels = base.nChannels;
        let bits = base.wBitsPerSample;
        assert_eq!(tag, WAVE_FORMAT_EXTENSIBLE as u16);
        assert_eq!((rate, channels), (48_000, 2));
        assert_eq!((bits, valid), (16, 16));
        assert_eq!(mask, SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT);
        assert_eq!(subtype, KSDATAFORMAT_SUBTYPE_PCM);
    }

    #[test]
    fn failure_at_each_role_rolls_back_every_displaced_role_already_changed() {
        let roles = [eConsole, eMultimedia, eCommunications];
        for failing_role in roles {
            let record = record([Some("old-0"), Some("old-1"), Some("old-2")]);
            let mut calls = Vec::new();
            let error = apply_default_transaction(&record, |id, role| {
                calls.push((id.to_owned(), role));
                if id == "cable" && role == failing_role {
                    Err(format!("injected failure at {role:?}"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert!(error.contains("injected failure"));
            let failed_index = roles.iter().position(|role| *role == failing_role).unwrap();
            for prior in 0..failed_index {
                assert!(
                    calls.contains(&(format!("old-{prior}"), roles[prior])),
                    "role {prior} was changed before {failing_role:?} and must be rolled back: {calls:?}"
                );
            }
        }
    }

    #[test]
    fn restore_changes_only_roles_still_owned_by_the_lease() {
        let record = record([Some("old-0"), Some("old-1"), Some("old-2")]);
        let current = [
            Some("cable".to_owned()),
            Some("user-choice".to_owned()),
            None,
        ];
        let mut calls = Vec::new();
        restore_owned_defaults(&current, &record, |id, role| {
            calls.push((id.to_owned(), role));
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, vec![("old-0".to_owned(), eConsole)]);
    }

    #[test]
    fn a_role_with_no_prior_default_is_set_but_not_restored() {
        let record = record([None, Some("old-1"), Some("old-2")]);
        let mut apply_calls = Vec::new();
        apply_default_transaction(&record, |id, role| {
            apply_calls.push((id.to_owned(), role));
            Ok(())
        })
        .unwrap();
        assert!(apply_calls.contains(&("cable".to_owned(), eConsole)));

        let current = [
            Some("cable".to_owned()),
            Some("cable".to_owned()),
            Some("cable".to_owned()),
        ];
        let mut restore_calls = Vec::new();
        restore_owned_defaults(&current, &record, |id, role| {
            restore_calls.push((id.to_owned(), role));
            Ok(())
        })
        .unwrap();
        assert!(!restore_calls.iter().any(|(_, role)| *role == eConsole));
        assert!(restore_calls.contains(&("old-1".to_owned(), eMultimedia)));
        assert!(restore_calls.contains(&("old-2".to_owned(), eCommunications)));
    }

    #[test]
    fn configure_sets_only_roles_that_have_no_default() {
        let current = [
            Some("speakers".to_owned()),
            None,
            Some("headset".to_owned()),
        ];
        let mut calls = Vec::new();
        set_missing_defaults(&current, "cable", |id, role| {
            calls.push((id.to_owned(), role));
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, vec![("cable".to_owned(), eMultimedia)]);
        assert!(!all_defaults_present(&current));
        assert!(all_defaults_present(&[
            Some("one".to_owned()),
            Some("two".to_owned()),
            Some("three".to_owned()),
        ]));
    }

    #[test]
    fn already_owned_roles_make_repeated_connect_idempotent() {
        let record = record([Some("cable"), Some("cable"), Some("cable")]);
        let mut calls = 0;
        apply_default_transaction(&record, |_, _| {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 0);
    }
}

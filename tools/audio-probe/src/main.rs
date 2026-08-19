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
fn main() -> std::process::ExitCode {
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
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

    // The decisive question. Loopback capture attaches to the default render
    // endpoint; if there is no default, the feature has nothing to capture.
    // SAFETY: a valid enumerator.
    let default = match unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) } {
        Ok(d) => {
            println!("audio-probe: default render endpoint: PRESENT");
            d
        }
        Err(e) => {
            println!("audio-probe: default render endpoint: NONE ({e})");
            println!("audio-probe: VERDICT — loopback capture cannot work on this host as configured.");
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
            println!("audio-probe: loopback client initialised");
            println!("audio-probe: VERDICT — loopback capture is viable on this host.");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            println!("audio-probe: loopback initialise failed: {e}");
            println!("audio-probe: VERDICT — loopback capture cannot work on this host as configured.");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("audio-probe: there is no WASAPI on this platform");
    std::process::ExitCode::from(2)
}

/*++

Copyright (c) Microsoft Corporation

Derived from microsoft/Windows-driver-samples@717778a2 (MIT) for the mdrdp latency spike.

Abstract:

    An indirect display driver that exposes ONE EDID-less virtual monitor capable of
    5120x2880 and 2560x1440 at 240/120/60 Hz, plus the original compatibility
    modes. It is a measurement instrument for the mdrdp latency spike, not a
    shipping display driver.

    Differences from the Microsoft sample this is derived from:
      * WPP tracing removed entirely (no Trace.h, no Driver.tmh).
      * One monitor, always EDID-less - the sample's static EDID table is gone.
      * Monitor/target mode lists retuned for high-refresh 5K and 1440p.
      * Frame-cadence instrumentation added to SwapChainProcessor::RunCore.
      * Each acquired frame is published to a user-mode server through a named shared
        texture pool - see SharedPool.h. That is the point of the driver now; the
        null-consumer loop it replaced survives only as the fallback for when the shared
        section cannot be created.

    MSDN documentation on indirect displays can be found at https://msdn.microsoft.com/en-us/library/windows/hardware/mt761968(v=vs.85).aspx.

Environment:

    User Mode, UMDF

--*/

#include "Driver.h"

using namespace std;
using namespace Microsoft::IndirectDisp;
using namespace Microsoft::WRL;

#pragma region Monitors

// One monitor, and it is always EDID-less: this is > ARRAYSIZE of any EDID table, so
// FinishInit always takes the descriptor-less path.
static constexpr DWORD MDRDP_IDD_MONITOR_COUNT = 1;

// Frames between cadence reports on the swap-chain thread.
static constexpr DWORD MDRDP_IDD_FRAME_LOG_INTERVAL = 600;

// Default modes reported for the EDID-less monitor. The first mode is set as preferred.
static const struct IndirectSampleMonitor::SampleMonitorMode s_MdrdpDefaultModes[] =
{
    { 5120, 2880, 240 },
    { 5120, 2880, 120 },
    { 5120, 2880,  60 },
    { 2560, 1440, 240 },
    { 2560, 1440, 120 },
    { 2560, 1440,  60 },
    { 1920, 1080, 240 },
    { 1920, 1080, 120 },
    { 1920, 1080,  60 },
};

#pragma endregion

#pragma region helpers

static inline void FillSignalInfo(DISPLAYCONFIG_VIDEO_SIGNAL_INFO& Mode, DWORD Width, DWORD Height, DWORD VSync, bool bMonitorMode)
{
    Mode.totalSize.cx = Mode.activeSize.cx = Width;
    Mode.totalSize.cy = Mode.activeSize.cy = Height;

    // See https://docs.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_video_signal_info
    Mode.AdditionalSignalInfo.vSyncFreqDivider = bMonitorMode ? 0 : 1;
    Mode.AdditionalSignalInfo.videoStandard = 255;

    Mode.vSyncFreq.Numerator = VSync;
    Mode.vSyncFreq.Denominator = 1;
    Mode.hSyncFreq.Numerator = VSync * Height;
    Mode.hSyncFreq.Denominator = 1;

    Mode.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;

    Mode.pixelRate = ((UINT64) VSync) * ((UINT64) Width) * ((UINT64) Height);
}

static IDDCX_MONITOR_MODE CreateIddCxMonitorMode(DWORD Width, DWORD Height, DWORD VSync, IDDCX_MONITOR_MODE_ORIGIN Origin = IDDCX_MONITOR_MODE_ORIGIN_DRIVER)
{
    IDDCX_MONITOR_MODE Mode = {};

    Mode.Size = sizeof(Mode);
    Mode.Origin = Origin;
    FillSignalInfo(Mode.MonitorVideoSignalInfo, Width, Height, VSync, true);

    return Mode;
}

static IDDCX_TARGET_MODE CreateIddCxTargetMode(DWORD Width, DWORD Height, DWORD VSync)
{
    IDDCX_TARGET_MODE Mode = {};

    Mode.Size = sizeof(Mode);
    FillSignalInfo(Mode.TargetVideoSignalInfo.targetVideoSignalInfo, Width, Height, VSync, false);

    return Mode;
}

#pragma endregion

extern "C" DRIVER_INITIALIZE DriverEntry;

EVT_WDF_DRIVER_DEVICE_ADD IddSampleDeviceAdd;
EVT_WDF_DEVICE_D0_ENTRY IddSampleDeviceD0Entry;

EVT_IDD_CX_ADAPTER_INIT_FINISHED IddSampleAdapterInitFinished;
EVT_IDD_CX_ADAPTER_COMMIT_MODES IddSampleAdapterCommitModes;

EVT_IDD_CX_PARSE_MONITOR_DESCRIPTION IddSampleParseMonitorDescription;
EVT_IDD_CX_MONITOR_GET_DEFAULT_DESCRIPTION_MODES IddSampleMonitorGetDefaultModes;
EVT_IDD_CX_MONITOR_QUERY_TARGET_MODES IddSampleMonitorQueryModes;

EVT_IDD_CX_MONITOR_ASSIGN_SWAPCHAIN IddSampleMonitorAssignSwapChain;
EVT_IDD_CX_MONITOR_UNASSIGN_SWAPCHAIN IddSampleMonitorUnassignSwapChain;

struct IndirectDeviceContextWrapper
{
    IndirectDeviceContext* pContext;

    void Cleanup()
    {
        delete pContext;
        pContext = nullptr;
    }
};

struct IndirectMonitorContextWrapper
{
    IndirectMonitorContext* pContext;

    void Cleanup()
    {
        delete pContext;
        pContext = nullptr;
    }
};

// This macro creates the methods for accessing an IndirectDeviceContextWrapper as a context for a WDF object
WDF_DECLARE_CONTEXT_TYPE(IndirectDeviceContextWrapper);

WDF_DECLARE_CONTEXT_TYPE(IndirectMonitorContextWrapper);

extern "C" BOOL WINAPI DllMain(
    _In_ HINSTANCE hInstance,
    _In_ UINT dwReason,
    _In_opt_ LPVOID lpReserved)
{
    UNREFERENCED_PARAMETER(hInstance);
    UNREFERENCED_PARAMETER(lpReserved);
    UNREFERENCED_PARAMETER(dwReason);

    return TRUE;
}

_Use_decl_annotations_
extern "C" NTSTATUS DriverEntry(
    PDRIVER_OBJECT  pDriverObject,
    PUNICODE_STRING pRegistryPath
)
{
    WDF_DRIVER_CONFIG Config;
    NTSTATUS Status;

    WDF_OBJECT_ATTRIBUTES Attributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&Attributes);

    WDF_DRIVER_CONFIG_INIT(&Config,
        IddSampleDeviceAdd
    );

    Status = WdfDriverCreate(pDriverObject, pRegistryPath, &Attributes, &Config, WDF_NO_HANDLE);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    return Status;
}

_Use_decl_annotations_
NTSTATUS IddSampleDeviceAdd(WDFDRIVER Driver, PWDFDEVICE_INIT pDeviceInit)
{
    NTSTATUS Status = STATUS_SUCCESS;
    WDF_PNPPOWER_EVENT_CALLBACKS PnpPowerCallbacks;

    UNREFERENCED_PARAMETER(Driver);

    // Register for power callbacks - in this sample only power-on is needed
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&PnpPowerCallbacks);
    PnpPowerCallbacks.EvtDeviceD0Entry = IddSampleDeviceD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(pDeviceInit, &PnpPowerCallbacks);

    IDD_CX_CLIENT_CONFIG IddConfig;
    IDD_CX_CLIENT_CONFIG_INIT(&IddConfig);

    // If the driver wishes to handle custom IoDeviceControl requests, it's necessary to use this callback since IddCx
    // redirects IoDeviceControl requests to an internal queue. This sample does not need this.
    // IddConfig.EvtIddCxDeviceIoControl = IddSampleIoDeviceControl;

    IddConfig.EvtIddCxAdapterInitFinished = IddSampleAdapterInitFinished;

    IddConfig.EvtIddCxParseMonitorDescription = IddSampleParseMonitorDescription;
    IddConfig.EvtIddCxMonitorGetDefaultDescriptionModes = IddSampleMonitorGetDefaultModes;
    IddConfig.EvtIddCxMonitorQueryTargetModes = IddSampleMonitorQueryModes;
    IddConfig.EvtIddCxAdapterCommitModes = IddSampleAdapterCommitModes;
    IddConfig.EvtIddCxMonitorAssignSwapChain = IddSampleMonitorAssignSwapChain;
    IddConfig.EvtIddCxMonitorUnassignSwapChain = IddSampleMonitorUnassignSwapChain;

    Status = IddCxDeviceInitConfig(pDeviceInit, &IddConfig);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    WDF_OBJECT_ATTRIBUTES Attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&Attr, IndirectDeviceContextWrapper);
    Attr.EvtCleanupCallback = [](WDFOBJECT Object)
    {
        // Automatically cleanup the context when the WDF object is about to be deleted
        auto* pContext = WdfObjectGet_IndirectDeviceContextWrapper(Object);
        if (pContext)
        {
            pContext->Cleanup();
        }
    };

    WDFDEVICE Device = nullptr;
    Status = WdfDeviceCreate(&pDeviceInit, &Attr, &Device);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    Status = IddCxDeviceInitialize(Device);

    // Create a new device context object and attach it to the WDF device object
    auto* pContext = WdfObjectGet_IndirectDeviceContextWrapper(Device);
    pContext->pContext = new IndirectDeviceContext(Device);

    return Status;
}

_Use_decl_annotations_
NTSTATUS IddSampleDeviceD0Entry(WDFDEVICE Device, WDF_POWER_DEVICE_STATE PreviousState)
{
    UNREFERENCED_PARAMETER(PreviousState);

    // This function is called by WDF to start the device in the fully-on power state.

    auto* pContext = WdfObjectGet_IndirectDeviceContextWrapper(Device);
    pContext->pContext->InitAdapter();

    return STATUS_SUCCESS;
}

#pragma region Direct3DDevice

Direct3DDevice::Direct3DDevice(LUID AdapterLuid) : AdapterLuid(AdapterLuid)
{

}

Direct3DDevice::Direct3DDevice()
{
    AdapterLuid = LUID{};
}

HRESULT Direct3DDevice::Init()
{
    // The DXGI factory could be cached, but if a new render adapter appears on the system, a new factory needs to be
    // created. If caching is desired, check DxgiFactory->IsCurrent() each time and recreate the factory if !IsCurrent.
    HRESULT hr = CreateDXGIFactory2(0, IID_PPV_ARGS(&DxgiFactory));
    if (FAILED(hr))
    {
        return hr;
    }

    // Find the specified render adapter
    hr = DxgiFactory->EnumAdapterByLuid(AdapterLuid, IID_PPV_ARGS(&Adapter));
    if (FAILED(hr))
    {
        return hr;
    }

    // Create a D3D device using the render adapter. BGRA support is required by the WHQL test suite.
    hr = D3D11CreateDevice(Adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0, D3D11_SDK_VERSION, &Device, nullptr, &DeviceContext);
    if (FAILED(hr))
    {
        // If creating the D3D device failed, it's possible the render GPU was lost (e.g. detachable GPU) or else the
        // system is in a transient state.
        return hr;
    }

    return S_OK;
}

#pragma endregion

#pragma region SwapChainProcessor

SwapChainProcessor::SwapChainProcessor(IDDCX_SWAPCHAIN hSwapChain, IDDCX_MONITOR Monitor, shared_ptr<Direct3DDevice> Device, HANDLE NewFrameEvent, shared_ptr<SharedSection> Section)
    : m_hSwapChain(hSwapChain),
      m_Monitor(Monitor),
      m_Device(Device),
      m_hAvailableBufferEvent(NewFrameEvent),
      m_LastCursorShapeId(0),
      m_Section(std::move(Section))
{
    // Both the frame and cursor workers observe termination, so this must be
    // manual-reset. The cursor event is auto-reset: one signal wakes one query.
    m_hTerminateEvent.Attach(CreateEvent(nullptr, TRUE, FALSE, nullptr));
    m_hCursorDataEvent.Attach(CreateEvent(nullptr, FALSE, FALSE, nullptr));

    // Immediately create and run the swap-chain processing thread, passing 'this' as the thread parameter
    m_hThread.Attach(CreateThread(nullptr, 0, RunThread, this, 0, nullptr));
}

SwapChainProcessor::~SwapChainProcessor()
{
    // Alert the swap-chain processing thread to terminate
    SetEvent(m_hTerminateEvent.Get());

    if (m_hThread.Get())
    {
        // Wait for the thread to terminate
        WaitForSingleObject(m_hThread.Get(), INFINITE);
    }

    if (m_hCursorThread.Get())
    {
        // The cursor worker shares the monitor and section lifetime with this object.
        WaitForSingleObject(m_hCursorThread.Get(), INFINITE);
    }
}

DWORD CALLBACK SwapChainProcessor::RunThread(LPVOID Argument)
{
    reinterpret_cast<SwapChainProcessor*>(Argument)->Run();
    return 0;
}

DWORD CALLBACK SwapChainProcessor::CursorThread(LPVOID Argument)
{
    reinterpret_cast<SwapChainProcessor*>(Argument)->ConsumeCursorUpdates();
    return 0;
}

void SwapChainProcessor::QueryHardwareCursor()
{
    IDARG_IN_QUERY_HWCURSOR QueryIn = {};
    QueryIn.LastShapeId = m_LastCursorShapeId;
    QueryIn.ShapeBufferSizeInBytes = sizeof(m_CursorShapeBuffer);
    QueryIn.pShapeBuffer = m_CursorShapeBuffer;

    IDARG_OUT_QUERY_HWCURSOR QueryOut = {};
    const NTSTATUS Status = IddCxMonitorQueryHardwareCursor(m_Monitor, &QueryIn, &QueryOut);
    if (!NT_SUCCESS(Status))
    {
        wchar_t Message[128];
        swprintf_s(Message, L"mdrdp-idd: IddCxMonitorQueryHardwareCursor failed, status=0x%08lx\n",
            static_cast<unsigned long>(Status));
        OutputDebugStringW(Message);
        if (m_Section != nullptr)
        {
            // A stale hidden state is worse than a visible default cursor: it can
            // leave the viewer with no immediate pointer at all.
            m_Section->PublishHardwareCursor(false);
        }
        return;
    }

    if (QueryOut.IsCursorShapeUpdated)
    {
        m_LastCursorShapeId = QueryOut.CursorShapeInfo.ShapeId;
    }

    // The cursor bitmap and position stay on the local platform. The shared word
    // carries only visibility, so a hidden Windows cursor no longer gets baked into
    // the delayed frame stream.
    if (m_Section != nullptr)
    {
        m_Section->PublishHardwareCursor(QueryOut.IsCursorVisible == FALSE);
    }

}

void SwapChainProcessor::ConsumeCursorUpdates()
{
    HANDLE WaitHandles[] =
    {
        m_hCursorDataEvent.Get(),
        m_hTerminateEvent.Get()
    };

    for (;;)
    {
        const DWORD WaitResult = WaitForMultipleObjects(ARRAYSIZE(WaitHandles), WaitHandles, FALSE, INFINITE);
        if (WaitResult == WAIT_OBJECT_0)
        {
            // Query once per notification. The auto-reset event preserves the
            // lifetime-safe worker boundary while allowing subsequent notifications
            // to wake the worker again immediately.
            QueryHardwareCursor();
        }
        else if (WaitResult == WAIT_OBJECT_0 + 1)
        {
            return;
        }
        else
        {
            return;
        }
    }
}

void SwapChainProcessor::Run()
{
    // For improved performance, make use of the Multimedia Class Scheduler Service, which will intelligently
    // prioritize this thread for improved throughput in high CPU-load scenarios.
    DWORD AvTask = 0;
    HANDLE AvTaskHandle = AvSetMmThreadCharacteristicsW(L"Distribution", &AvTask);

    RunCore();

    // Always delete the swap-chain object when swap-chain processing loop terminates in order to kick the system to
    // provide a new swap-chain if necessary.
    WdfObjectDelete((WDFOBJECT)m_hSwapChain);
    m_hSwapChain = nullptr;

    AvRevertMmThreadCharacteristics(AvTaskHandle);
}

void SwapChainProcessor::RunCore()
{
    // Get the DXGI device interface
    ComPtr<IDXGIDevice> DxgiDevice;
    HRESULT hr = m_Device->Device.As(&DxgiDevice);
    if (FAILED(hr))
    {
        return;
    }

    IDARG_IN_SWAPCHAINSETDEVICE SetDevice = {};
    SetDevice.pDevice = DxgiDevice.Get();

    hr = IddCxSwapChainSetDevice(m_hSwapChain, &SetDevice);
    if (FAILED(hr))
    {
        return;
    }

    // Claim the cursor plane before the first surface is acquired. IddCx keeps
    // unsupported shapes in software composition; the full alpha/XOR capability
    // and 256x256 bounds cover the cursor classes used by the 200% desktop.
    if (m_hCursorDataEvent.Get() != nullptr)
    {
        IDARG_IN_SETUP_HWCURSOR CursorSetup = {};
        CursorSetup.CursorInfo.Size = sizeof(CursorSetup.CursorInfo);
        CursorSetup.CursorInfo.AlphaCursorSupport = TRUE;
        CursorSetup.CursorInfo.ColorXorCursorSupport = IDDCX_XOR_CURSOR_SUPPORT_FULL;
        CursorSetup.CursorInfo.MaxX = MDRDP_IDD_CURSOR_MAX_X;
        CursorSetup.CursorInfo.MaxY = MDRDP_IDD_CURSOR_MAX_Y;
        CursorSetup.hNewCursorDataAvailable = m_hCursorDataEvent.Get();

        const NTSTATUS CursorStatus = IddCxMonitorSetupHardwareCursor(m_Monitor, &CursorSetup);
        if (NT_SUCCESS(CursorStatus))
        {
            // Seed visibility immediately. Waiting for the first change event can
            // leave a newly connected viewer with a stale default state.
            QueryHardwareCursor();
            m_hCursorThread.Attach(CreateThread(nullptr, 0, CursorThread, this, 0, nullptr));
            if (m_hCursorThread.Get() == nullptr)
            {
                OutputDebugStringW(L"mdrdp-idd: failed to create the hardware cursor worker\n");
            }
        }
        else
        {
            wchar_t Message[128];
            swprintf_s(Message, L"mdrdp-idd: IddCxMonitorSetupHardwareCursor failed, status=0x%08lx\n",
                static_cast<unsigned long>(CursorStatus));
            OutputDebugStringW(Message);
        }
    }

    // mdrdp cadence instrumentation. Nothing here allocates or touches the filesystem:
    // a stack buffer, swprintf_s, and one OutputDebugStringW every
    // MDRDP_IDD_FRAME_LOG_INTERVAL acquired frames.
    LARGE_INTEGER PerfFrequency = {};
    LARGE_INTEGER WindowStart = {};
    QueryPerformanceFrequency(&PerfFrequency);
    QueryPerformanceCounter(&WindowStart);
    DWORD FramesThisWindow = 0;

    // The shared pool is built lazily, on the first frame: its textures must match the
    // acquired surface, whose width/height/format are not known until then. An unusable
    // section (creation failed, or the well-known name was squatted) leaves the pool
    // permanently unstarted, and the loop below degrades to the null consumer it used to
    // be rather than failing the display.
    const bool PoolWanted = (m_Section != nullptr) && m_Section->Usable();
    bool PoolStartTried = false;

    // Acquire and release buffers in a loop
    for (;;)
    {
        ComPtr<IDXGIResource> AcquiredBuffer;

        // Ask for the next buffer from the producer
        IDARG_OUT_RELEASEANDACQUIREBUFFER Buffer = {};
        hr = IddCxSwapChainReleaseAndAcquireBuffer(m_hSwapChain, &Buffer);

        // AcquireBuffer immediately returns STATUS_PENDING if no buffer is yet available
        if (hr == E_PENDING)
        {
            // We must wait for a new buffer
            HANDLE WaitHandles [] =
            {
                m_hAvailableBufferEvent,
                m_hTerminateEvent.Get()
            };
            DWORD WaitResult = WaitForMultipleObjects(ARRAYSIZE(WaitHandles), WaitHandles, FALSE, 16);
            if (WaitResult == WAIT_OBJECT_0 || WaitResult == WAIT_TIMEOUT)
            {
                // We have a new buffer, so try the AcquireBuffer again
                continue;
            }
            else if (WaitResult == WAIT_OBJECT_0 + 1)
            {
                // We need to terminate
                break;
            }
            else
            {
                // The wait was cancelled or something unexpected happened
                hr = HRESULT_FROM_WIN32(WaitResult);
                break;
            }
        }
        else if (SUCCEEDED(hr))
        {
            // We have new frame to process, the surface has a reference on it that the driver has to release
            AcquiredBuffer.Attach(Buffer.MetaData.pSurface);

            if (PoolWanted)
            {
                ComPtr<ID3D11Texture2D> AcquiredTexture;
                if (SUCCEEDED(AcquiredBuffer.As(&AcquiredTexture)))
                {
                    if (!PoolStartTried)
                    {
                        // Once only. A failure is reported by the pool and then left alone:
                        // retrying every frame would spam the log and burn a generation
                        // each time. The next swap-chain assignment is the natural retry.
                        PoolStartTried = true;

                        D3D11_TEXTURE2D_DESC SourceDesc = {};
                        AcquiredTexture->GetDesc(&SourceDesc);

                        m_Pool.Start(m_Section.get(), m_Device->Device.Get(), m_Device->DeviceContext.Get(), m_Device->AdapterLuid, SourceDesc);
                    }

                    // Copies into the next slot if the server is not holding it, folds this
                    // frame's dirty/move coverage into all three either way, and signals the
                    // slot's event. Never blocks: see SharedPool.h.
                    m_Pool.ProcessFrame(m_hSwapChain, Buffer.MetaData, AcquiredTexture.Get());
                }
            }

            // We have finished processing this frame hence we release the reference on it.
            // If the driver forgets to release the reference to the surface, it will be leaked which results in the
            // surfaces being left around after swapchain is destroyed.
            AcquiredBuffer.Reset();

            // Indicate to OS that we have finished inital processing of the frame, it is a hint that
            // OS could start preparing another frame
            hr = IddCxSwapChainFinishedProcessingFrame(m_hSwapChain);
            if (FAILED(hr))
            {
                break;
            }

            if (++FramesThisWindow >= MDRDP_IDD_FRAME_LOG_INTERVAL)
            {
                LARGE_INTEGER Now = {};
                QueryPerformanceCounter(&Now);

                double Seconds = 0.0;
                if (PerfFrequency.QuadPart != 0)
                {
                    Seconds = static_cast<double>(Now.QuadPart - WindowStart.QuadPart) / static_cast<double>(PerfFrequency.QuadPart);
                }

                // Publish counters ride the same report: how many of those frames actually
                // reached a slot, how many lost their slot to the server, and what the copy
                // ordering cost on the present path. The HLD budgets that cost against the
                // 7.5 ms this pool removes, so it is measured, not asserted free.
                SharedFramePool::WindowStats Stats = {};
                m_Pool.TakeWindowStats(Stats);

                wchar_t Message[256];
                swprintf_s(Message,
                    L"mdrdp-idd: %lu frames in %.2f s (%.1f fps), published %lu, slot-busy %lu, copy-wait avg %.3f ms max %.3f ms, wait-timeouts %lu\n",
                    static_cast<unsigned long>(FramesThisWindow),
                    Seconds,
                    (Seconds > 0.0) ? (static_cast<double>(FramesThisWindow) / Seconds) : 0.0,
                    static_cast<unsigned long>(Stats.Published),
                    static_cast<unsigned long>(Stats.SkippedBusy),
                    (Stats.Published > 0) ? (Stats.CopyWaitSumMs / static_cast<double>(Stats.Published)) : 0.0,
                    Stats.CopyWaitMaxMs,
                    static_cast<unsigned long>(Stats.CopyWaitTimeouts));
                OutputDebugStringW(Message);

                FramesThisWindow = 0;
                WindowStart = Now;
            }
        }
        else
        {
            // The swap-chain was likely abandoned (e.g. DXGI_ERROR_ACCESS_LOST), so exit the processing loop
            break;
        }
    }
}

#pragma endregion

#pragma region IndirectDeviceContext

IndirectDeviceContext::IndirectDeviceContext(_In_ WDFDEVICE WdfDevice) :
    m_WdfDevice(WdfDevice)
{
    m_Adapter = {};
}

IndirectDeviceContext::~IndirectDeviceContext()
{
}

void IndirectDeviceContext::InitAdapter()
{
    // Claim the one well-known name before any monitor exists, and hold it for the life of
    // the device: the swap-chain pool is rebuilt on every assignment, but a server must be
    // able to find the section across those rebuilds. Idempotent, because D0Entry can fire
    // more than once. If it cannot be claimed, the swap-chain loop publishes nothing and
    // the display still works.
    if (m_SharedSection == nullptr)
    {
        m_SharedSection = std::make_shared<SharedSection>();
    }
    m_SharedSection->Create();

    // The strings and version numbers below are used for telemetry and may be displayed to
    // the user in some situations. This is also where static per-adapter capabilities are
    // determined.

    IDDCX_ADAPTER_CAPS AdapterCaps = {};
    AdapterCaps.Size = sizeof(AdapterCaps);
    // Pre-IddCx 1.7 only: without this flag Windows folds moves into dirty rects.
    // Current runtimes ignore the deprecated flag and report complete dirty pixels,
    // which the server's exact inference path handles instead.
    AdapterCaps.Flags = IDDCX_ADAPTER_FLAGS_CAN_USE_MOVE_REGIONS;

    // Declare basic feature support for the adapter (required)
    AdapterCaps.MaxMonitorsSupported = MDRDP_IDD_MONITOR_COUNT;
    AdapterCaps.EndPointDiagnostics.Size = sizeof(AdapterCaps.EndPointDiagnostics);
    AdapterCaps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    AdapterCaps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_WIRED_OTHER;

    // Declare your device strings for telemetry (required)
    AdapterCaps.EndPointDiagnostics.pEndPointFriendlyName = L"mdrdp latency-spike display";
    AdapterCaps.EndPointDiagnostics.pEndPointManufacturerName = L"mdrdp";
    AdapterCaps.EndPointDiagnostics.pEndPointModelName = L"mdrdp-idd";

    // Declare your hardware and firmware versions (required)
    IDDCX_ENDPOINT_VERSION Version = {};
    Version.Size = sizeof(Version);
    Version.MajorVer = 1;
    AdapterCaps.EndPointDiagnostics.pFirmwareVersion = &Version;
    AdapterCaps.EndPointDiagnostics.pHardwareVersion = &Version;

    // Initialize a WDF context that can store a pointer to the device context object
    WDF_OBJECT_ATTRIBUTES Attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&Attr, IndirectDeviceContextWrapper);

    IDARG_IN_ADAPTER_INIT AdapterInit = {};
    AdapterInit.WdfDevice = m_WdfDevice;
    AdapterInit.pCaps = &AdapterCaps;
    AdapterInit.ObjectAttributes = &Attr;

    // Start the initialization of the adapter, which will trigger the AdapterFinishInit callback later
    IDARG_OUT_ADAPTER_INIT AdapterInitOut;
    NTSTATUS Status = IddCxAdapterInitAsync(&AdapterInit, &AdapterInitOut);

    if (NT_SUCCESS(Status))
    {
        // Store a reference to the WDF adapter handle
        m_Adapter = AdapterInitOut.AdapterObject;

        // Store the device context object into the WDF object context
        auto* pContext = WdfObjectGet_IndirectDeviceContextWrapper(AdapterInitOut.AdapterObject);
        pContext->pContext = this;
    }
}

void IndirectDeviceContext::FinishInit(UINT ConnectorIndex)
{
    WDF_OBJECT_ATTRIBUTES Attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&Attr, IndirectMonitorContextWrapper);

    IDDCX_MONITOR_INFO MonitorInfo = {};
    MonitorInfo.Size = sizeof(MonitorInfo);
    MonitorInfo.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI;
    MonitorInfo.ConnectorIndex = ConnectorIndex;

    MonitorInfo.MonitorDescription.Size = sizeof(MonitorInfo.MonitorDescription);
    MonitorInfo.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;

    // This derivation reports exactly one EDID-less monitor: the sample's
    // "ConnectorIndex >= ARRAYSIZE(s_SampleMonitors)" branch, unconditionally. Its modes
    // therefore come from EvtIddCxMonitorGetDefaultDescriptionModes, not from an EDID.
    MonitorInfo.MonitorDescription.DataSize = 0;
    MonitorInfo.MonitorDescription.pData = nullptr;

    // The monitor's container ID should be distinct from "this" device's container ID because the
    // monitor is not permanently attached to the display adapter device object.
    CoCreateGuid(&MonitorInfo.MonitorContainerId);

    IDARG_IN_MONITORCREATE MonitorCreate = {};
    MonitorCreate.ObjectAttributes = &Attr;
    MonitorCreate.pMonitorInfo = &MonitorInfo;

    // Create a monitor object with the specified monitor descriptor
    IDARG_OUT_MONITORCREATE MonitorCreateOut;
    NTSTATUS Status = IddCxMonitorCreate(m_Adapter, &MonitorCreate, &MonitorCreateOut);
    if (NT_SUCCESS(Status))
    {
        // Create a new monitor context object and attach it to the Idd monitor object
        auto* pMonitorContextWrapper = WdfObjectGet_IndirectMonitorContextWrapper(MonitorCreateOut.MonitorObject);
        pMonitorContextWrapper->pContext = new IndirectMonitorContext(MonitorCreateOut.MonitorObject, m_SharedSection);

        // Tell the OS that the monitor has been plugged in
        IDARG_OUT_MONITORARRIVAL ArrivalOut;
        Status = IddCxMonitorArrival(MonitorCreateOut.MonitorObject, &ArrivalOut);
    }
}

IndirectMonitorContext::IndirectMonitorContext(_In_ IDDCX_MONITOR Monitor, std::shared_ptr<SharedSection> Section) :
    m_Monitor(Monitor),
    m_Section(std::move(Section))
{
}

IndirectMonitorContext::~IndirectMonitorContext()
{
    m_ProcessingThread.reset();
}

void IndirectMonitorContext::AssignSwapChain(IDDCX_SWAPCHAIN SwapChain, LUID RenderAdapter, HANDLE NewFrameEvent)
{
    m_ProcessingThread.reset();

    auto Device = make_shared<Direct3DDevice>(RenderAdapter);
    if (FAILED(Device->Init()))
    {
        // It's important to delete the swap-chain if D3D initialization fails, so that the OS knows to generate a new
        // swap-chain and try again.
        WdfObjectDelete(SwapChain);
    }
    else
    {
        // Create a new swap-chain processing thread
        m_ProcessingThread.reset(new SwapChainProcessor(SwapChain, m_Monitor, Device, NewFrameEvent, m_Section));
    }
}

void IndirectMonitorContext::UnassignSwapChain()
{
    // Stop processing the last swap-chain
    m_ProcessingThread.reset();
}

#pragma endregion

#pragma region DDI Callbacks

_Use_decl_annotations_
NTSTATUS IddSampleAdapterInitFinished(IDDCX_ADAPTER AdapterObject, const IDARG_IN_ADAPTER_INIT_FINISHED* pInArgs)
{
    // This is called when the OS has finished setting up the adapter for use by the IddCx driver. It's now possible
    // to report attached monitors.

    auto* pDeviceContextWrapper = WdfObjectGet_IndirectDeviceContextWrapper(AdapterObject);
    if (NT_SUCCESS(pInArgs->AdapterInitStatus))
    {
        for (DWORD i = 0; i < MDRDP_IDD_MONITOR_COUNT; i++)
        {
            pDeviceContextWrapper->pContext->FinishInit(i);
        }
    }

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddSampleAdapterCommitModes(IDDCX_ADAPTER AdapterObject, const IDARG_IN_COMMITMODES* pInArgs)
{
    UNREFERENCED_PARAMETER(AdapterObject);
    UNREFERENCED_PARAMETER(pInArgs);

    // Nothing to do when modes are picked - the swap-chain is taken care of by IddCx, and this
    // virtual monitor has no hardware to reconfigure.

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddSampleParseMonitorDescription(const IDARG_IN_PARSEMONITORDESCRIPTION* pInArgs, IDARG_OUT_PARSEMONITORDESCRIPTION* pOutArgs)
{
    UNREFERENCED_PARAMETER(pInArgs);
    UNREFERENCED_PARAMETER(pOutArgs);

    // Unreachable in this driver: IddCx only calls EvtIddCxParseMonitorDescription for a monitor
    // that was created WITH a descriptor, and FinishInit always reports DataSize = 0 / pData =
    // nullptr. The callback stays wired because IddCx requires it to be non-null in
    // IDD_CX_CLIENT_CONFIG; a description we never supplied can only be one we do not know.
    return STATUS_INVALID_PARAMETER;
}

_Use_decl_annotations_
NTSTATUS IddSampleMonitorGetDefaultModes(IDDCX_MONITOR MonitorObject, const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* pInArgs, IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* pOutArgs)
{
    UNREFERENCED_PARAMETER(MonitorObject);

    // This is where the EDID-less monitor's modes come from. Index 0 is the preferred mode.

    if (pInArgs->DefaultMonitorModeBufferInputCount == 0)
    {
        pOutArgs->DefaultMonitorModeBufferOutputCount = ARRAYSIZE(s_MdrdpDefaultModes);
    }
    else
    {
        for (DWORD ModeIndex = 0; ModeIndex < ARRAYSIZE(s_MdrdpDefaultModes); ModeIndex++)
        {
            pInArgs->pDefaultMonitorModes[ModeIndex] = CreateIddCxMonitorMode(
                s_MdrdpDefaultModes[ModeIndex].Width,
                s_MdrdpDefaultModes[ModeIndex].Height,
                s_MdrdpDefaultModes[ModeIndex].VSync,
                IDDCX_MONITOR_MODE_ORIGIN_DRIVER
            );
        }

        pOutArgs->DefaultMonitorModeBufferOutputCount = ARRAYSIZE(s_MdrdpDefaultModes);
        pOutArgs->PreferredMonitorModeIdx = 0;
    }

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddSampleMonitorQueryModes(IDDCX_MONITOR MonitorObject, const IDARG_IN_QUERYTARGETMODES* pInArgs, IDARG_OUT_QUERYTARGETMODES* pOutArgs)
{
    UNREFERENCED_PARAMETER(MonitorObject);

    vector<IDDCX_TARGET_MODE> TargetModes;

    // Create a set of modes supported for frame processing and scan-out. These are typically not based on the
    // monitor's descriptor and instead are based on the static processing capability of the device. The OS will
    // report the available set of modes for a given output as the intersection of monitor modes with target modes.

    TargetModes.push_back(CreateIddCxTargetMode(5120, 2880, 240));
    TargetModes.push_back(CreateIddCxTargetMode(5120, 2880, 120));
    TargetModes.push_back(CreateIddCxTargetMode(5120, 2880,  60));
    TargetModes.push_back(CreateIddCxTargetMode(2560, 1440, 240));
    TargetModes.push_back(CreateIddCxTargetMode(2560, 1440, 120));
    TargetModes.push_back(CreateIddCxTargetMode(2560, 1440,  60));
    TargetModes.push_back(CreateIddCxTargetMode(1920, 1080, 240));
    TargetModes.push_back(CreateIddCxTargetMode(1920, 1080, 120));
    TargetModes.push_back(CreateIddCxTargetMode(1920, 1080,  60));

    pOutArgs->TargetModeBufferOutputCount = (UINT) TargetModes.size();

    if (pInArgs->TargetModeBufferInputCount >= TargetModes.size())
    {
        copy(TargetModes.begin(), TargetModes.end(), pInArgs->pTargetModes);
    }

    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddSampleMonitorAssignSwapChain(IDDCX_MONITOR MonitorObject, const IDARG_IN_SETSWAPCHAIN* pInArgs)
{
    auto* pMonitorContextWrapper = WdfObjectGet_IndirectMonitorContextWrapper(MonitorObject);
    pMonitorContextWrapper->pContext->AssignSwapChain(pInArgs->hSwapChain, pInArgs->RenderAdapterLuid, pInArgs->hNextSurfaceAvailable);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS IddSampleMonitorUnassignSwapChain(IDDCX_MONITOR MonitorObject)
{
    auto* pMonitorContextWrapper = WdfObjectGet_IndirectMonitorContextWrapper(MonitorObject);
    pMonitorContextWrapper->pContext->UnassignSwapChain();
    return STATUS_SUCCESS;
}

#pragma endregion

/*++

Copyright (c) mdrdp contributors

Abstract:

    Implementation of the mdrdp shared frame pool. See SharedPool.h for the section layout,
    which is the cross-language contract, and for the coverage invariant, which is the part
    that is easy to get subtly wrong.

    Nothing in this file allocates on the frame path, touches the filesystem, or logs
    anything but fixed diagnostic text: it runs on the swap-chain thread, where a stall is
    back-pressure on DWM.

Environment:

    User Mode, UMDF

--*/

#include "SharedPool.h"

#include <sddl.h>
#include <bcrypt.h>
#include <cstdio>

using namespace Microsoft::IndirectDisp;
using namespace Microsoft::WRL;

#pragma region helpers

// How long the swap-chain thread will wait for the GPU to finish a slot copy before giving
// up on it. The wait holds the slot's keyed mutex, so it must sit UNDER the consumer's own
// 4 ms mutex timeout as well as under the 4.17 ms frame interval of the 240 Hz mode this
// driver advertises - a 1080p CopyResource is ~0.2 ms, so 2 ms is already ten times the
// common case. If we ever hit it, the log says so and the frame is not published (a copy
// that never provably finished would publish torn pixels as a complete frame).
static constexpr LONGLONG MDRDP_IDD_COPY_WAIT_LIMIT_US = 2000;

// Seqlock, writer side. The sequence is odd while a write is in flight and even when the
// record is stable, so a reader that samples the same even value either side of its read
// knows nothing changed underneath it.
static inline void SeqlockBegin(volatile UINT32* pSequence)
{
    *pSequence = *pSequence + 1;
    MemoryBarrier();
}

// Adopted memory may carry an ODD sequence word: the previous driver instance can have
// died between its SeqlockBegin and SeqlockEnd. Writing through an odd word inverts the
// lock's parity permanently - stable-and-odd reads as mid-write forever, and the reader
// gives up on the header (or retires a slot) for the life of the pool. Every writer that
// touches memory it did not itself initialise normalises first (review, D3).
static inline void SeqlockNormalize(volatile UINT32* pSequence)
{
    if ((*pSequence % 2) != 0)
    {
        *pSequence = *pSequence + 1;
        MemoryBarrier();
    }
}

static inline void SeqlockEnd(volatile UINT32* pSequence)
{
    MemoryBarrier();
    *pSequence = *pSequence + 1;
}

static void LogHresult(const wchar_t* What, HRESULT hr)
{
    wchar_t Message[160];
    swprintf_s(Message, L"mdrdp-idd: %ls failed, hr=0x%08lx\n", What, static_cast<unsigned long>(hr));
    OutputDebugStringW(Message);
}

// A random per-generation name suffix. Fixed names would collide with a straggling
// server's still-open handles on a pool rebuild (DXGI_ERROR_NAME_ALREADY_EXISTS) and would
// invite pre-creation squatting; a random one makes each generation's objects unguessable
// and unambiguously new.
static UINT64 MintNameSuffix()
{
    UINT64 Suffix = 0;

    NTSTATUS Status = BCryptGenRandom(
        nullptr,
        reinterpret_cast<PUCHAR>(&Suffix),
        sizeof(Suffix),
        BCRYPT_USE_SYSTEM_PREFERRED_RNG);

    if (!BCRYPT_SUCCESS(Status) || Suffix == 0)
    {
        // Uniqueness, not unpredictability, is what the names strictly need; QPC plus the
        // host PID gives that much even if the RNG is unavailable.
        LARGE_INTEGER Now = {};
        QueryPerformanceCounter(&Now);
        Suffix = (static_cast<UINT64>(GetCurrentProcessId()) << 32) ^ static_cast<UINT64>(Now.QuadPart);
        Suffix |= 1;
    }

    return Suffix;
}

#pragma endregion

#pragma region SharedSection

SharedSection::SharedSection() :
    m_pView(nullptr),
    m_pDescriptor(nullptr),
    m_LastGeneration(0)
{
    m_SecurityAttributes = {};
}

SharedSection::~SharedSection()
{
    if (m_pView)
    {
        UnmapViewOfFile(m_pView);
        m_pView = nullptr;
    }

    m_hSection.Close();

    if (m_pDescriptor)
    {
        LocalFree(m_pDescriptor);
        m_pDescriptor = nullptr;
    }
}

void SharedSection::Create()
{
    // Idempotent: EvtDeviceD0Entry fires on every power transition, and the section is
    // meant to outlive all of them. A refused name is retried on the next call rather
    // than latched: the common holder of a stale name is a still-running server from
    // before a driver redeploy, and that clears itself.
    if (m_pView != nullptr)
    {
        return;
    }

    if (m_pDescriptor == nullptr)
    {
        if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
                MDRDP_IDD_SDDL, SDDL_REVISION_1, &m_pDescriptor, nullptr))
        {
            LogHresult(L"ConvertStringSecurityDescriptorToSecurityDescriptorW",
                HRESULT_FROM_WIN32(GetLastError()));
            return;
        }

        m_SecurityAttributes.nLength = sizeof(m_SecurityAttributes);
        m_SecurityAttributes.lpSecurityDescriptor = m_pDescriptor;
        m_SecurityAttributes.bInheritHandle = FALSE;
    }

    HANDLE hSection = CreateFileMappingW(
        INVALID_HANDLE_VALUE,
        &m_SecurityAttributes,
        PAGE_READWRITE,
        0,
        MDRDP_IDD_SECTION_BYTES,
        MDRDP_IDD_SECTION_NAME);

    const DWORD Error = GetLastError();

    if (hSection == nullptr)
    {
        LogHresult(L"CreateFileMappingW(Global\\mdrdp-idd)", HRESULT_FROM_WIN32(Error));
        return;
    }

    m_hSection.Attach(hSection);

    m_pView = static_cast<BYTE*>(MapViewOfFile(
        m_hSection.Get(), FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, MDRDP_IDD_SECTION_BYTES));

    if (m_pView == nullptr)
    {
        // Includes the squatter-with-a-smaller-section case: a view of our full size
        // cannot be mapped over an object that is not shaped like ours.
        LogHresult(L"MapViewOfFile(Global\\mdrdp-idd)", HRESULT_FROM_WIN32(GetLastError()));
        m_hSection.Close();
        return;
    }

    MdrdpSharedHeader* pHeader = Header();

    if (Error == ERROR_ALREADY_EXISTS)
    {
        // Somebody already holds the one well-known name. The overwhelmingly common
        // holder is our own previous section, kept alive by a still-running server's
        // open handle across a driver redeploy - creating in Global\ needs
        // SeCreateGlobalPrivilege, so an actual squatter must already be privileged.
        // Adopt it if it is shaped like ours (version 0 = zero-filled and never
        // stamped, version 1 = ours); refuse anything else, and retry on a later
        // D0Entry rather than latching off forever.
        const UINT32 Version = pHeader->LayoutVersion;
        if (Version != 0 && Version != MDRDP_IDD_LAYOUT_VERSION)
        {
            UnmapViewOfFile(m_pView);
            m_pView = nullptr;
            m_hSection.Close();
            OutputDebugStringW(L"mdrdp-idd: Global\\mdrdp-idd already existed with a foreign layout - SHARED POOL DISABLED for now (squatting?); will retry on the next power-up\n");
            return;
        }

        // Generations must keep moving forward through the adopted header so a consumer
        // holding the old pool sees the next build as a bump.
        m_LastGeneration = pHeader->Generation;
        OutputDebugStringW(L"mdrdp-idd: adopted the existing Global\\mdrdp-idd section (a consumer held it across a driver restart)\n");
    }

    // Stamp the version and slot count (a fresh section is zero-filled), and force the
    // generation to 0: whatever pool the header used to advertise, its textures and
    // events died with the driver instance that owned them, and 0 is the header state a
    // consumer already treats as "no pool yet - wait".
    SeqlockNormalize(&pHeader->HeaderSequence);
    SeqlockBegin(&pHeader->HeaderSequence);
    pHeader->LayoutVersion = MDRDP_IDD_LAYOUT_VERSION;
    pHeader->SlotCount = MDRDP_IDD_SLOT_COUNT;
    pHeader->Generation = 0;
    SeqlockEnd(&pHeader->HeaderSequence);

    OutputDebugStringW(L"mdrdp-idd: shared section Global\\mdrdp-idd ready\n");
}

// The next pool generation. Monotonic across every pool build this driver instance
// performs, and seeded from an adopted header so it stays monotonic across the driver
// restart a held-open section survives.
UINT32 SharedSection::NextGeneration()
{
    return ++m_LastGeneration;
}

// The header state a consumer treats as "no pool - wait". Called when a pool is torn
// down, because a header that keeps advertising a dead generation strands a consumer
// that opens mid-gap on names whose objects no longer exist.
void SharedSection::AdvertiseNoPool()
{
    if (m_pView == nullptr)
    {
        return;
    }

    MdrdpSharedHeader* pHeader = Header();
    SeqlockBegin(&pHeader->HeaderSequence);
    pHeader->Generation = 0;
    SeqlockEnd(&pHeader->HeaderSequence);
}

MdrdpSharedSlot* SharedSection::Slot(UINT32 Index) const
{
    if (m_pView == nullptr || Index >= MDRDP_IDD_SLOT_COUNT)
    {
        return nullptr;
    }

    return reinterpret_cast<MdrdpSharedSlot*>(m_pView + MDRDP_IDD_SLOT_STRIDE * (1 + Index));
}

#pragma endregion

#pragma region SharedFramePool

SharedFramePool::SharedFramePool() :
    m_pSection(nullptr),
    m_FrameSeq(0),
    m_Started(false)
{
    m_PerfFrequency = {};
    m_Stats = {};

    for (UINT32 i = 0; i < MDRDP_IDD_SLOT_COUNT; i++)
    {
        m_Slots[i].LastPublishedFrameSeq = 0;
        m_Coverage[i] = {};
    }
}

SharedFramePool::~SharedFramePool()
{
    Stop();
}

HRESULT SharedFramePool::Start(
    SharedSection* pSection,
    ID3D11Device* pDevice,
    ID3D11DeviceContext* pContext,
    LUID RenderAdapter,
    const D3D11_TEXTURE2D_DESC& SourceDesc)
{
    Stop();

    if (pSection == nullptr || !pSection->Usable() || pDevice == nullptr || pContext == nullptr)
    {
        return E_INVALIDARG;
    }

    QueryPerformanceFrequency(&m_PerfFrequency);
    if (m_PerfFrequency.QuadPart == 0)
    {
        // Cannot happen on any supported Windows, but a zero frequency would turn the
        // copy-wait limit into "give up instantly", which now means "publish nothing,
        // ever" - fail the pool loudly instead of running it as a no-op (review, D7).
        LogHresult(L"QueryPerformanceFrequency", E_FAIL);
        return E_FAIL;
    }

    m_pSection = pSection;
    m_Device = pDevice;
    m_Context = pContext;
    m_Stats = {};

    // The generation the consumer watches for invalidation. It only ever goes up, and it
    // lives with the section rather than in this object because the section outlives
    // every swap-chain assignment. It is NOT read back from the header: teardown
    // advertises generation 0 there, and a counter that dipped would let two distinct
    // pools share a generation number.
    const UINT32 Generation = pSection->NextGeneration();
    const UINT64 Suffix = MintNameSuffix();

    // Match the acquired surface exactly - the copy is a straight CopyResource, so any
    // mismatch is a silent failure rather than a conversion.
    D3D11_TEXTURE2D_DESC Desc = {};
    Desc.Width = SourceDesc.Width;
    Desc.Height = SourceDesc.Height;
    Desc.MipLevels = 1;
    Desc.ArraySize = 1;
    Desc.Format = SourceDesc.Format;
    Desc.SampleDesc.Count = 1;
    Desc.SampleDesc.Quality = 0;
    Desc.Usage = D3D11_USAGE_DEFAULT;
    Desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    Desc.CPUAccessFlags = 0;
    Desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX;

    for (UINT32 i = 0; i < MDRDP_IDD_SLOT_COUNT; i++)
    {
        HRESULT hr = m_Device->CreateTexture2D(&Desc, nullptr, &m_Slots[i].Texture);
        if (FAILED(hr))
        {
            LogHresult(L"CreateTexture2D(pool slot)", hr);
            Stop();
            return hr;
        }

        ComPtr<IDXGIResource1> Resource;
        hr = m_Slots[i].Texture.As(&Resource);
        if (FAILED(hr))
        {
            LogHresult(L"QI IDXGIResource1(pool slot)", hr);
            Stop();
            return hr;
        }

        wchar_t Name[96];
        swprintf_s(Name, L"Global\\mdrdp-idd-tex-%lu-%016llx-%lu",
            static_cast<unsigned long>(Generation),
            static_cast<unsigned long long>(Suffix),
            static_cast<unsigned long>(i));

        HANDLE hShared = nullptr;
        hr = Resource->CreateSharedHandle(
            pSection->Attributes(),
            DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE,
            Name,
            &hShared);
        if (FAILED(hr))
        {
            LogHresult(L"CreateSharedHandle(pool slot)", hr);
            Stop();
            return hr;
        }
        m_Slots[i].hShared.Attach(hShared);

        hr = m_Slots[i].Texture.As(&m_Slots[i].Mutex);
        if (FAILED(hr))
        {
            LogHresult(L"QI IDXGIKeyedMutex(pool slot)", hr);
            Stop();
            return hr;
        }

        swprintf_s(Name, L"Global\\mdrdp-idd-evt-%lu-%016llx-%lu",
            static_cast<unsigned long>(Generation),
            static_cast<unsigned long long>(Suffix),
            static_cast<unsigned long>(i));

        HANDLE hEvent = CreateEventW(pSection->Attributes(), FALSE, FALSE, Name);
        if (hEvent == nullptr)
        {
            const HRESULT hrEvent = HRESULT_FROM_WIN32(GetLastError());
            LogHresult(L"CreateEventW(pool slot)", hrEvent);
            Stop();
            return hrEvent;
        }
        m_Slots[i].hEvent.Attach(hEvent);

        m_Slots[i].LastPublishedFrameSeq = 0;
        m_Coverage[i] = {};
    }

    D3D11_QUERY_DESC QueryDesc = {};
    QueryDesc.Query = D3D11_QUERY_EVENT;

    HRESULT hr = m_Device->CreateQuery(&QueryDesc, &m_CopyFence);
    if (FAILED(hr))
    {
        LogHresult(L"CreateQuery(D3D11_QUERY_EVENT)", hr);
        Stop();
        return hr;
    }

    // Wipe every slot record BEFORE the new generation becomes visible. The records
    // carry no generation field, and the previous pool's final frame numbers are still
    // in these pages - a consumer that adopted the new generation and then read a stale
    // record would latch its frame counter onto a number the new pool will not reach
    // for as long as the old one ran, and go silent for exactly that long (review, C1).
    // FrameSeq 0 is a state the consumer's slot selection already filters out.
    for (UINT32 i = 0; i < MDRDP_IDD_SLOT_COUNT; i++)
    {
        MdrdpSharedSlot* pSlot = pSection->Slot(i);
        if (pSlot == nullptr)
        {
            continue;
        }
        volatile UINT32* pSlotSequence = reinterpret_cast<volatile UINT32*>(&pSlot->Sequence);
        SeqlockNormalize(pSlotSequence);
        SeqlockBegin(pSlotSequence);
        pSlot->Generation = Generation;
        pSlot->FrameSeq = 0;
        pSlot->DirtySinceFrameSeq = 0;
        pSlot->PresentQpc = 0;
        pSlot->CoverageRectCount = MDRDP_IDD_COVERAGE_ABSENT;
        SeqlockEnd(pSlotSequence);
    }

    // Published LAST, and only now: a generation a server can see is a generation whose
    // textures and events already exist under the names derived from it, behind slot
    // records that are provably not another generation's.
    MdrdpSharedHeader* pHeader = pSection->Header();
    SeqlockBegin(&pHeader->HeaderSequence);
    pHeader->LayoutVersion = MDRDP_IDD_LAYOUT_VERSION;
    pHeader->Generation = Generation;
    pHeader->RenderAdapterLuid =
        static_cast<UINT64>(static_cast<UINT32>(RenderAdapter.LowPart)) |
        (static_cast<UINT64>(static_cast<UINT32>(RenderAdapter.HighPart)) << 32);
    pHeader->Width = Desc.Width;
    pHeader->Height = Desc.Height;
    pHeader->DxgiFormat = static_cast<UINT32>(Desc.Format);
    pHeader->SlotCount = MDRDP_IDD_SLOT_COUNT;
    pHeader->NameSuffix = Suffix;
    pHeader->Reserved = 0;
    SeqlockEnd(&pHeader->HeaderSequence);


    m_Generation = Generation;
    m_FrameSeq = 0;
    m_Started = true;

    wchar_t Message[192];
    swprintf_s(Message, L"mdrdp-idd: shared pool gen %lu suffix %016llx, %lux%lu fmt %lu, %lu slots\n",
        static_cast<unsigned long>(Generation),
        static_cast<unsigned long long>(Suffix),
        static_cast<unsigned long>(Desc.Width),
        static_cast<unsigned long>(Desc.Height),
        static_cast<unsigned long>(Desc.Format),
        static_cast<unsigned long>(MDRDP_IDD_SLOT_COUNT));
    OutputDebugStringW(Message);

    return S_OK;
}

void SharedFramePool::Stop()
{
    // First word out: "no pool". A header that kept advertising this generation would
    // strand a consumer that opens mid-gap on names whose objects die with the ComPtrs
    // below (review, M3). Generation 0 is the state the consumer waits out.
    if (m_pSection != nullptr)
    {
        m_pSection->AdvertiseNoPool();
    }

    for (UINT32 i = 0; i < MDRDP_IDD_SLOT_COUNT; i++)
    {
        m_Slots[i].Mutex.Reset();
        m_Slots[i].Texture.Reset();
        m_Slots[i].hShared.Close();
        m_Slots[i].hEvent.Close();
        m_Slots[i].LastPublishedFrameSeq = 0;
        m_Coverage[i] = {};
    }

    m_CopyFence.Reset();
    m_Context.Reset();
    m_Device.Reset();

    m_pSection = nullptr;
    m_FrameSeq = 0;
    m_Started = false;
}

void SharedFramePool::ProcessFrame(
    IDDCX_SWAPCHAIN hSwapChain,
    const IDDCX_METADATA& MetaData,
    ID3D11Texture2D* pSource)
{
    if (!m_Started || pSource == nullptr)
    {
        return;
    }

    LARGE_INTEGER Acquired = {};
    QueryPerformanceCounter(&Acquired);

    // Counts every presented frame, published or not, so the sequence a consumer sees is
    // contiguous and a gap in it means exactly one thing: a frame it did not get.
    m_FrameSeq++;

    // Fold before publish. This frame's own coverage belongs both in the record it is about
    // to produce and in the two records the other slots will produce later.
    FoldFrameCoverage(hSwapChain, MetaData);

    const UINT32 Index = static_cast<UINT32>(m_FrameSeq % MDRDP_IDD_SLOT_COUNT);
    Slot& Target = m_Slots[Index];

    const INT64 PresentQpc = (MetaData.PresentDisplayQPCTime != 0)
        ? static_cast<INT64>(MetaData.PresentDisplayQPCTime)
        : Acquired.QuadPart;

    // Zero timeout, always. A swap-chain thread that waits on the server back-pressures
    // DWM, which is precisely the latency this pool exists to delete - so a slot the server
    // still holds simply loses this frame. WAIT_ABANDONED is a SUCCEEDED HRESULT and does
    // mean we own the mutex, so it takes the acquired path and must be released.
    const HRESULT hrAcquire = Target.Mutex->AcquireSync(0, 0);
    if (hrAcquire != S_OK && hrAcquire != static_cast<HRESULT>(WAIT_ABANDONED))
    {
        m_Stats.SkippedBusy++;
        return;
    }

    m_Context->CopyResource(Target.Texture.Get(), pSource);

    // The keyed mutex protects the destination; NOTHING protects the source, which goes
    // back to the swap-chain as soon as we return. Order the copy against that or DWM
    // composites into a surface the GPU is still reading. A copy that never provably
    // finished is NOT published - torn pixels under a record that calls them complete
    // would defeat the whole contract; the frame is simply lost and counted.
    const bool CopyProven = WaitForCopy();

    if (CopyProven)
    {
        // The record is written INSIDE the mutex, before the release: the pixels and the
        // record that describes them must be one atomic unit to any holder. Published
        // after the release, a consumer that acquired the instant we let go could pair
        // this frame's pixels with the PREVIOUS record - and its coverage list would
        // then under-claim, which the client's exactness invariant turns into
        // permanently stale canvas regions. The consumer re-reads the record under the
        // same mutex.
        PublishSlot(Index, m_FrameSeq, PresentQpc);
    }
    else
    {
        // The copy already clobbered the surface; leaving the PREVIOUS record standing
        // would advertise the torn pixels under an older frame's truthful-looking
        // coverage (review, D4). Invalidate the record - but leave the accumulator and
        // LastPublishedFrameSeq alone, so the next real publish still describes a
        // truthful union that includes this lost frame.
        MdrdpSharedSlot* pSlot = m_pSection->Slot(Index);
        if (pSlot != nullptr)
        {
            volatile UINT32* pSequence = reinterpret_cast<volatile UINT32*>(&pSlot->Sequence);
            SeqlockBegin(pSequence);
            pSlot->Generation = m_Generation;
            pSlot->FrameSeq = 0;
            pSlot->CoverageRectCount = MDRDP_IDD_COVERAGE_ABSENT;
            SeqlockEnd(pSequence);
        }
    }

    Target.Mutex->ReleaseSync(0);

    if (CopyProven)
    {
        SetEvent(Target.hEvent.Get());
        m_Stats.Published++;
    }
}

void SharedFramePool::TakeWindowStats(WindowStats& Stats)
{
    Stats = m_Stats;
    m_Stats = {};
}

#pragma endregion

#pragma region SharedFramePool - coverage

void SharedFramePool::FoldFrameCoverage(IDDCX_SWAPCHAIN hSwapChain, const IDDCX_METADATA& MetaData)
{
    // Zero dirty rects AND zero move regions is not missing metadata: the OS reports it for
    // a re-present of an unchanged desktop. "Nothing changed" folds nothing, and that is
    // information the consumer is entitled to.
    if (MetaData.DirtyRectCount > MDRDP_IDD_MAX_COVERAGE_RECTS ||
        MetaData.MoveRegionCount > MDRDP_IDD_MAX_COVERAGE_RECTS)
    {
        // More than the scratch buffers can retrieve in one go, so this frame's union
        // cannot be completed at all.
        MarkAllOverflowed();
        return;
    }

    if (MetaData.DirtyRectCount != 0)
    {
        IDARG_IN_GETDIRTYRECTS In = {};
        In.DirtyRectInCount = MetaData.DirtyRectCount;
        In.pDirtyRects = m_ScratchRects;

        IDARG_OUT_GETDIRTYRECTS Out = {};
        if (FAILED(IddCxSwapChainGetDirtyRects(hSwapChain, &In, &Out)))
        {
            MarkAllAbsent();
            return;
        }

        for (UINT32 i = 0; i < Out.DirtyRectOutCount && i < MDRDP_IDD_MAX_COVERAGE_RECTS; i++)
        {
            AppendCoverageRect(m_ScratchRects[i]);
        }
    }

    if (MetaData.MoveRegionCount != 0)
    {
        for (UINT32 i = 0; i < MetaData.MoveRegionCount; i++)
        {
            m_ScratchMoves[i] = {};
            m_ScratchMoves[i].Size = sizeof(IDDCX_MOVEREGION);
        }

        IDARG_IN_GETMOVEREGIONS In = {};
        In.MoveRegionInCount = MetaData.MoveRegionCount;
        In.pMoveRegions = m_ScratchMoves;

        IDARG_OUT_GETMOVEREGIONS Out = {};
        if (FAILED(IddCxSwapChainGetMoveRegions(hSwapChain, &In, &Out)))
        {
            MarkAllAbsent();
            return;
        }

        // We transport FINAL PIXELS, so a move's destination rect is the whole of what a
        // consumer has to re-read; the source rect and the ordering are irrelevant to us.
        // That is what makes an accumulated union complete rather than merely plausible
        // (HLD decision 17) - coverage is order-free, a replay of moves would not be.
        for (UINT32 i = 0; i < Out.MoveRegionOutCount && i < MDRDP_IDD_MAX_COVERAGE_RECTS; i++)
        {
            AppendCoverageRect(m_ScratchMoves[i].DestRect);
        }
    }
}

void SharedFramePool::AppendCoverageRect(const RECT& Rect)
{
    // Every frame folds into EVERY slot: the accumulators are what let a record describe
    // the whole span since that slot last published, not just the frame that filled it.
    // No coalescing, no merging - append, cap, done.
    for (UINT32 i = 0; i < MDRDP_IDD_SLOT_COUNT; i++)
    {
        CoverageAccumulator& Accumulator = m_Coverage[i];

        if (Accumulator.Absent)
        {
            continue;
        }

        if (Accumulator.Count >= MDRDP_IDD_MAX_COVERAGE_RECTS)
        {
            Accumulator.Overflowed = true;
            continue;
        }

        Accumulator.Rects[Accumulator.Count++] = Rect;
    }
}

void SharedFramePool::MarkAllOverflowed()
{
    for (UINT32 i = 0; i < MDRDP_IDD_SLOT_COUNT; i++)
    {
        m_Coverage[i].Overflowed = true;
    }
}

void SharedFramePool::MarkAllAbsent()
{
    // A hole in the metadata breaks the union outright: there is no honest rect list that
    // describes a span containing a frame we could not describe. Every slot is poisoned
    // until it next publishes, which clears it.
    for (UINT32 i = 0; i < MDRDP_IDD_SLOT_COUNT; i++)
    {
        m_Coverage[i].Absent = true;
    }
}

bool SharedFramePool::WaitForCopy()
{
    m_Context->End(m_CopyFence.Get());
    m_Context->Flush();

    LARGE_INTEGER Start = {};
    QueryPerformanceCounter(&Start);

    const LONGLONG Limit = (m_PerfFrequency.QuadPart != 0)
        ? (m_PerfFrequency.QuadPart * MDRDP_IDD_COPY_WAIT_LIMIT_US) / 1000000
        : 0;

    bool Signalled = false;
    LARGE_INTEGER Now = Start;

    for (;;)
    {
        BOOL Complete = FALSE;
        const HRESULT hr = m_Context->GetData(
            m_CopyFence.Get(), &Complete, sizeof(Complete), D3D11_ASYNC_GETDATA_DONOTFLUSH);

        QueryPerformanceCounter(&Now);

        if (hr == S_OK && Complete)
        {
            Signalled = true;
            break;
        }

        if (FAILED(hr) || (Now.QuadPart - Start.QuadPart) >= Limit)
        {
            // Give up rather than hold the swap-chain thread. The caller still releases the
            // keyed mutex, so a stuck GPU costs a torn slot, not a wedged display.
            m_Stats.CopyWaitTimeouts++;
            break;
        }

        YieldProcessor();
    }

    if (m_PerfFrequency.QuadPart != 0)
    {
        const double WaitMs =
            (static_cast<double>(Now.QuadPart - Start.QuadPart) * 1000.0) /
            static_cast<double>(m_PerfFrequency.QuadPart);

        m_Stats.CopyWaitSumMs += WaitMs;
        if (WaitMs > m_Stats.CopyWaitMaxMs)
        {
            m_Stats.CopyWaitMaxMs = WaitMs;
        }
    }

    return Signalled;
}

void SharedFramePool::PublishSlot(UINT32 Index, UINT64 FrameSeq, INT64 PresentQpc)
{
    MdrdpSharedSlot* pSlot = m_pSection->Slot(Index);
    if (pSlot == nullptr)
    {
        return;
    }

    CoverageAccumulator& Accumulator = m_Coverage[Index];

    // Absent outranks overflow: if any frame in this span had no metadata at all, no rect
    // list can be complete regardless of how few rects the rest of the span produced.
    UINT32 Count = Accumulator.Count;
    if (Accumulator.Absent)
    {
        Count = MDRDP_IDD_COVERAGE_ABSENT;
    }
    else if (Accumulator.Overflowed)
    {
        Count = MDRDP_IDD_COVERAGE_OVERFLOW;
    }

    volatile UINT32* pSequence = reinterpret_cast<volatile UINT32*>(&pSlot->Sequence);

    SeqlockBegin(pSequence);
    pSlot->Generation = m_Generation;
    pSlot->FrameSeq = FrameSeq;
    // The range this record's rects cover is (DirtySinceFrameSeq, FrameSeq]. A consumer
    // whose last consumed frame is at or after DirtySinceFrameSeq may treat the list as a
    // complete delta; one that stalled further back must not, and must take the whole
    // surface instead.
    pSlot->DirtySinceFrameSeq = m_Slots[Index].LastPublishedFrameSeq;
    pSlot->PresentQpc = PresentQpc;
    pSlot->CoverageRectCount = Count;
    pSlot->Reserved1 = 0;
    for (UINT32 i = 0; i < Accumulator.Count; i++)
    {
        pSlot->CoverageRects[i] = Accumulator.Rects[i];
    }
    SeqlockEnd(pSequence);

    m_Slots[Index].LastPublishedFrameSeq = FrameSeq;
    Accumulator = {};
}

#pragma endregion

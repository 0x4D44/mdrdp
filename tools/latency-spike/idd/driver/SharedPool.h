/*++

Copyright (c) mdrdp contributors

Abstract:

    The driver half of the mdrdp shared frame pool: a named shared-memory section plus a
    3-slot ring of named shared D3D11 textures and named events, through which the
    swap-chain thread hands each committed frame straight to a user-mode server. This is
    Increment 2 of "HLD - native low-latency transport" (2026.08.17), whose point is to
    delete Desktop Duplication's ~7.5 ms present-to-acquire gap: the driver already holds
    the buffer at the earliest possible moment, so a copy plus a signal replaces it.

    The section layout below IS the cross-language contract - a Rust consumer is written
    against these exact byte offsets. Change a field and you change both sides.

Environment:

    User Mode, UMDF

--*/

#pragma once

#ifndef NOMINMAX          // build.sh already defines it on the command line
#define NOMINMAX
#endif
#include <windows.h>
#include <wudfwdm.h>
#include <wdf.h>
// Spelled with the on-disk casing, for the same reason Driver.h is - see build.sh.
#include <IddCx.h>

#include <dxgi1_5.h>
#include <d3d11_2.h>
#include <wrl.h>

namespace Microsoft
{
    namespace WRL
    {
        namespace Wrappers
        {
            // The pool holds section, event and shared-texture NT handles. All three are
            // NULL-on-failure, so they take the same wrapper the sample gives threads.
            typedef HandleT<HandleTraits::HANDLENullTraits> NullHandle;
        }
    }
}

namespace Microsoft
{
    namespace IndirectDisp
    {
#pragma region Shared section contract

        // The one well-known name in the design. Everything else is generation-qualified
        // with a random suffix and published through this section's header, so a
        // straggling server's open handles can never collide with a rebuilt pool.
        static constexpr wchar_t MDRDP_IDD_SECTION_NAME[] = L"Global\\mdrdp-idd";

        // Every named object the driver creates gets this DACL: full control to LocalSystem,
        // to administrators, and to the interactive user (the server runs as the latter).
        static constexpr wchar_t MDRDP_IDD_SDDL[] = L"D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)";

        static constexpr DWORD  MDRDP_IDD_SECTION_BYTES = 16384;
        static constexpr DWORD  MDRDP_IDD_SLOT_STRIDE = 4096;
        static constexpr UINT32 MDRDP_IDD_LAYOUT_VERSION = 1;
        static constexpr UINT32 MDRDP_IDD_SLOT_COUNT = 3;
        static constexpr UINT32 MDRDP_IDD_MAX_COVERAGE_RECTS = 64;

        // Two reserved values in coverage_rect_count. ABSENT means "we never had this
        // frame's metadata", which is deliberately distinct from a genuine zero-rect frame
        // (the OS reports zero dirty AND zero move regions for a re-present of an unchanged
        // desktop, and that is real information). OVERFLOW means metadata existed but did
        // not fit the 64-entry cap; the consumer treats it as absent.
        static constexpr UINT32 MDRDP_IDD_COVERAGE_ABSENT = 0xFFFFFFFFu;
        static constexpr UINT32 MDRDP_IDD_COVERAGE_OVERFLOW = 0xFFFFFFFEu;

        /// <summary>
        /// Section header, at offset 0. Written under a seqlock: HeaderSequence is odd
        /// while a write is in flight and even when the rest of the struct is stable.
        /// </summary>
        struct MdrdpSharedHeader
        {
            UINT32 LayoutVersion;       //  0 - == MDRDP_IDD_LAYOUT_VERSION
            UINT32 Generation;          //  4 - 0 = no pool yet; bumps on every pool build
            UINT64 RenderAdapterLuid;   //  8 - LowPart | ((UINT64)HighPart << 32)
            UINT32 Width;               // 16
            UINT32 Height;              // 20
            UINT32 DxgiFormat;          // 24 - DXGI_FORMAT of the pool textures
            UINT32 SlotCount;           // 28 - == MDRDP_IDD_SLOT_COUNT
            UINT64 NameSuffix;          // 32 - random per generation; names are built from it
            UINT32 HeaderSequence;      // 40 - seqlock
            UINT32 Reserved;            // 44 - 0
        };

        /// <summary>
        /// One slot record, at offset MDRDP_IDD_SLOT_STRIDE * (1 + index). Same seqlock
        /// discipline as the header.
        /// </summary>
        struct MdrdpSharedSlot
        {
            UINT32 Sequence;            //  0 - seqlock
            UINT32 Reserved0;           //  4 - 0
            UINT64 FrameSeq;            //  8 - contiguous presented-frame counter, from 1
            UINT64 DirtySinceFrameSeq;  // 16 - see the coverage invariant below
            INT64  PresentQpc;          // 24 - QPC ticks
            UINT32 CoverageRectCount;   // 32 - or MDRDP_IDD_COVERAGE_ABSENT / _OVERFLOW
            UINT32 Reserved1;           // 36 - 0
            RECT   CoverageRects[MDRDP_IDD_MAX_COVERAGE_RECTS];  // 40 - 1024 bytes
        };

        // The contract is byte offsets, not "whatever the compiler picked today".
        static_assert(sizeof(MdrdpSharedHeader) == 48, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, Generation) == 4, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, RenderAdapterLuid) == 8, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, Width) == 16, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, Height) == 20, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, DxgiFormat) == 24, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, SlotCount) == 28, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, NameSuffix) == 32, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, HeaderSequence) == 40, "header layout drifted");
        static_assert(offsetof(MdrdpSharedHeader, Reserved) == 44, "header layout drifted");

        static_assert(sizeof(RECT) == 16, "RECT is not 4 x i32");
        static_assert(sizeof(MdrdpSharedSlot) == 1064, "slot layout drifted");
        static_assert(offsetof(MdrdpSharedSlot, FrameSeq) == 8, "slot layout drifted");
        static_assert(offsetof(MdrdpSharedSlot, DirtySinceFrameSeq) == 16, "slot layout drifted");
        static_assert(offsetof(MdrdpSharedSlot, PresentQpc) == 24, "slot layout drifted");
        static_assert(offsetof(MdrdpSharedSlot, CoverageRectCount) == 32, "slot layout drifted");
        static_assert(offsetof(MdrdpSharedSlot, CoverageRects) == 40, "slot layout drifted");
        static_assert(MDRDP_IDD_SLOT_STRIDE * (1 + MDRDP_IDD_SLOT_COUNT) == MDRDP_IDD_SECTION_BYTES,
            "the section must hold the header page plus one page per slot");

#pragma endregion

        /// <summary>
        /// The one well-known named section. Created once at adapter start and held open
        /// across swap-chain assignments, because its name is the only thing a server can
        /// find without being told. `ERROR_ALREADY_EXISTS` usually means our own previous
        /// section, kept alive by a consumer's open handle across a driver restart - that
        /// is adopted if its layout validates as ours (creating in `Global\` needs
        /// SeCreateGlobalPrivilege, so a genuine squatter is already privileged); a
        /// foreign layout is refused, loudly, and retried on the next power-up.
        /// </summary>
        class SharedSection
        {
        public:
            SharedSection();
            ~SharedSection();

            // Idempotent: D0Entry can fire more than once over a device's life. A name
            // held by a foreign object is retried on the next call, never latched off.
            void Create();

            bool Usable() const { return m_pView != nullptr; }

            // The SD every texture and event in the pool is created with, so the server
            // that can open the section can open everything it names.
            SECURITY_ATTRIBUTES* Attributes() { return m_pDescriptor ? &m_SecurityAttributes : nullptr; }

            MdrdpSharedHeader* Header() const { return reinterpret_cast<MdrdpSharedHeader*>(m_pView); }
            MdrdpSharedSlot* Slot(UINT32 Index) const;

            // Monotonic across pool builds AND across a driver restart whose section a
            // consumer held open (the adopted header seeds it).
            UINT32 NextGeneration();

            // Header generation back to 0 - "no pool, wait" - so a torn-down pool never
            // strands a late-opening consumer on names whose objects are gone.
            void AdvertiseNoPool();

        private:
            SharedSection(const SharedSection&) = delete;
            SharedSection& operator=(const SharedSection&) = delete;

            Microsoft::WRL::Wrappers::NullHandle m_hSection;
            BYTE* m_pView;
            PSECURITY_DESCRIPTOR m_pDescriptor;
            SECURITY_ATTRIBUTES m_SecurityAttributes;
            UINT32 m_LastGeneration;
        };

        /// <summary>
        /// The per-assignment pool: 3 shared textures, 3 named events, and the coverage
        /// bookkeeping that makes each published record self-describing.
        ///
        /// THE COVERAGE INVARIANT. One accumulator per slot, and EVERY frame's dirty-union-
        /// move-destination rects fold into ALL THREE. When a frame publishes into slot i,
        /// the record carries accumulator[i]'s rects and DirtySinceFrameSeq = the FrameSeq
        /// of the previous successful publish into that same slot; the accumulator is then
        /// emptied. The record's rect list is therefore the complete coverage of the
        /// half-open range (DirtySinceFrameSeq, FrameSeq] - so a consumer whose last
        /// consumed frame is >= DirtySinceFrameSeq may treat the list as a complete delta,
        /// and one that stalled further back must not. Skipped frames (server holding the
        /// keyed mutex) still fold, so the union never develops a hole. Coverage is
        /// order-free because we transport final pixels, which is why folding move
        /// DESTINATION rects into the dirty set is enough (HLD decision 17).
        /// </summary>
        class SharedFramePool
        {
        public:
            /// <summary>Window counters for the swap-chain thread's cadence report.</summary>
            struct WindowStats
            {
                UINT32 Published;
                UINT32 SkippedBusy;      // keyed mutex held by the server
                UINT32 CopyWaitTimeouts;
                double CopyWaitMaxMs;
                double CopyWaitSumMs;
            };

            SharedFramePool();
            ~SharedFramePool();

            // Builds the pool against an already-acquired surface's description, because
            // that is the first moment the width/height/format are known. Bumps the
            // section's generation and publishes the header last, so a server never sees a
            // generation whose textures do not yet exist.
            HRESULT Start(
                SharedSection* pSection,
                ID3D11Device* pDevice,
                ID3D11DeviceContext* pContext,
                LUID RenderAdapter,
                const D3D11_TEXTURE2D_DESC& SourceDesc);

            void Stop();
            bool Started() const { return m_Started; }

            // One acquired frame: fold its coverage, then publish it into slot
            // FrameSeq % 3 if the server is not holding that slot's keyed mutex.
            void ProcessFrame(
                IDDCX_SWAPCHAIN hSwapChain,
                const IDDCX_METADATA& MetaData,
                ID3D11Texture2D* pSource);

            // Reads and clears the window counters.
            void TakeWindowStats(WindowStats& Stats);

        private:
            SharedFramePool(const SharedFramePool&) = delete;
            SharedFramePool& operator=(const SharedFramePool&) = delete;

            struct Slot
            {
                Microsoft::WRL::ComPtr<ID3D11Texture2D> Texture;
                Microsoft::WRL::ComPtr<IDXGIKeyedMutex> Mutex;
                Microsoft::WRL::Wrappers::NullHandle hShared;   // the named NT handle
                Microsoft::WRL::Wrappers::NullHandle hEvent;
                UINT64 LastPublishedFrameSeq;
            };

            // A fixed accumulator - the frame path allocates nothing.
            struct CoverageAccumulator
            {
                UINT32 Count;
                bool Overflowed;
                bool Absent;
                RECT Rects[MDRDP_IDD_MAX_COVERAGE_RECTS];
            };

            void FoldFrameCoverage(IDDCX_SWAPCHAIN hSwapChain, const IDDCX_METADATA& MetaData);
            void AppendCoverageRect(const RECT& Rect);
            void MarkAllOverflowed();
            void MarkAllAbsent();
            bool WaitForCopy();
            void PublishSlot(UINT32 Index, UINT64 FrameSeq, INT64 PresentQpc);

            SharedSection* m_pSection;
            Microsoft::WRL::ComPtr<ID3D11Device> m_Device;
            Microsoft::WRL::ComPtr<ID3D11DeviceContext> m_Context;
            Microsoft::WRL::ComPtr<ID3D11Query> m_CopyFence;
            Slot m_Slots[MDRDP_IDD_SLOT_COUNT];
            CoverageAccumulator m_Coverage[MDRDP_IDD_SLOT_COUNT];

            // Scratch for the two IddCx metadata queries. Fixed, never reallocated.
            RECT m_ScratchRects[MDRDP_IDD_MAX_COVERAGE_RECTS];
            IDDCX_MOVEREGION m_ScratchMoves[MDRDP_IDD_MAX_COVERAGE_RECTS];

            LARGE_INTEGER m_PerfFrequency;
            UINT64 m_FrameSeq;
            bool m_Started;

            WindowStats m_Stats;
        };
    }
}

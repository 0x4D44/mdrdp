/*
 * A flat C surface over the two FreeRDP primitives this oracle needs.
 *
 * Rust does not bind `primitives_t` — it is a large struct of function pointers
 * whose layout changes between FreeRDP releases, and getting it wrong would fail
 * silently rather than loudly. Calling through C keeps the ABI knowledge in the
 * compiler that owns the header.
 *
 * This file embeds no FreeRDP source; it calls the installed library through its
 * public headers.
 */

#include <stdint.h>

#include <freerdp/primitives.h>
#include <freerdp/codec/color.h>

static primitives_t* pick(int generic)
{
	return generic ? primitives_get_generic() : primitives_get();
}

/* Which implementation answered: PRIM_FLAGS_HAVE_EXTCPU means a SIMD build. */
uint32_t avc444diff_flags(int generic)
{
	primitives_t* prims = pick(generic);
	if (!prims)
		return 0xFFFFFFFFu;
	return (uint32_t)prims->flags;
}

/*
 * Bit 0: primitives_get() and primitives_get_generic() share one
 *        YUV420CombineToYUV444. Bit 1: likewise for YUV444ToRGB_8u_P3AC4R.
 *
 * Without this, a green run against both tables could mean the optimized table
 * simply points at the generic function and only one implementation was ever
 * tested.
 */
uint32_t avc444diff_shared_impls(void)
{
	primitives_t* fast = primitives_get();
	primitives_t* slow = primitives_get_generic();
	uint32_t bits = 0;

	if (!fast || !slow)
		return 0xFFFFFFFFu;

	if (fast->YUV420CombineToYUV444 == slow->YUV420CombineToYUV444)
		bits |= 1u;
	if (fast->YUV444ToRGB_8u_P3AC4R == slow->YUV444ToRGB_8u_P3AC4R)
		bits |= 2u;
	return bits;
}

/*
 * type: 0 = AVC444_LUMA, 1 = AVC444_CHROMAv1, 2 = AVC444_CHROMAv2.
 * n_width / n_height are the "total" dimensions and are only read by the v2 pass.
 * The rect is half-open: right and bottom are exclusive.
 */
int avc444diff_combine(int generic, int type, const uint8_t* y, const uint8_t* u, const uint8_t* v,
                       const uint32_t src_step[3], uint32_t n_width, uint32_t n_height, uint8_t* dy,
                       uint8_t* du, uint8_t* dv, const uint32_t dst_step[3], uint16_t l, uint16_t t,
                       uint16_t r, uint16_t b)
{
	primitives_t* prims = pick(generic);
	const BYTE* pSrc[3] = { (const BYTE*)y, (const BYTE*)u, (const BYTE*)v };
	BYTE* pDst[3] = { (BYTE*)dy, (BYTE*)du, (BYTE*)dv };
	RECTANGLE_16 roi;

	if (!prims || !prims->YUV420CombineToYUV444)
		return -1000;

	roi.left = l;
	roi.top = t;
	roi.right = r;
	roi.bottom = b;

	return (int)prims->YUV420CombineToYUV444((avc444_frame_type)type, pSrc, src_step, n_width,
	                                         n_height, pDst, dst_step, &roi);
}

/*
 * PIXEL_FORMAT_RGBA32 puts red at byte 0, green at 1, blue at 2, alpha at 3 —
 * the same order our `to_rgba_into` produces. FreeRDP's YUV path asks for the
 * writer with useAlpha = FALSE, so for this format it uses writePixelRGBX and
 * leaves the alpha byte untouched; the caller must pre-fill the destination with
 * 0xFF for the comparison against our constant-0xFF alpha to be meaningful.
 */
int avc444diff_to_rgb(int generic, const uint8_t* y, const uint8_t* u, const uint8_t* v,
                      const uint32_t src_step[3], uint8_t* dst, uint32_t dst_step, uint32_t width,
                      uint32_t height)
{
	primitives_t* prims = pick(generic);
	const BYTE* pSrc[3] = { (const BYTE*)y, (const BYTE*)u, (const BYTE*)v };
	prim_size_t roi;

	if (!prims || !prims->YUV444ToRGB_8u_P3AC4R)
		return -1000;

	roi.width = width;
	roi.height = height;

	return (int)prims->YUV444ToRGB_8u_P3AC4R(pSrc, src_step, (BYTE*)dst, dst_step,
	                                         PIXEL_FORMAT_RGBA32, &roi);
}

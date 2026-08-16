// Third-party code: see tools/NOTICE for provenance and licence.
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
typedef int16_t INT16;
typedef int32_t INT32;
#define WINPR_RESTRICT restrict
#define INT16_MIN_ (-32768)
#define INT16_MAX_ (32767)

static int16_t clampi16(int val)
{
	if (val < -32768) return -32768;
	if (val > 32767) return 32767;
	return (int16_t)val;
}

static inline void progressive_rfx_idwt_x(const INT16* WINPR_RESTRICT pLowBand, size_t nLowStep,
                                          const INT16* WINPR_RESTRICT pHighBand, size_t nHighStep,
                                          INT16* WINPR_RESTRICT pDstBand, size_t nDstStep,
                                          size_t nLowCount, size_t nHighCount, size_t nDstCount)
{
	INT16 H1 = 0;
	INT16 X1 = 0;

	for (size_t i = 0; i < nDstCount; i++)
	{
		const INT16* pL = pLowBand;
		const INT16* pH = pHighBand;
		INT16* pX = pDstBand;
		INT16 H0 = *pH++;
		INT16 L0 = *pL++;
		INT16 X0 = clampi16((int32_t)L0 - H0);
		INT16 X2 = clampi16((int32_t)L0 - H0);

		for (size_t j = 0; j < (nHighCount - 1); j++)
		{
			H1 = *pH; pH++;
			L0 = *pL; pL++;
			X2 = clampi16((int32_t)L0 - ((H0 + H1) / 2));
			X1 = clampi16((int32_t)((X0 + X2) / 2) + (2 * H0));
			pX[0] = X0;
			pX[1] = X1;
			pX += 2;
			X0 = X2;
			H0 = H1;
		}

		if (nLowCount <= (nHighCount + 1))
		{
			if (nLowCount <= nHighCount)
			{
				pX[0] = X2;
				pX[1] = clampi16((int32_t)X2 + (2 * H0));
			}
			else
			{
				L0 = *pL; pL++;
				X0 = clampi16((int32_t)L0 - H0);
				pX[0] = X2;
				pX[1] = clampi16((int32_t)((X0 + X2) / 2) + (2 * H0));
				pX[2] = X0;
			}
		}
		else
		{
			L0 = *pL; pL++;
			X0 = clampi16((int32_t)L0 - (H0 / 2));
			pX[0] = X2;
			pX[1] = clampi16((int32_t)((X0 + X2) / 2) + (2 * H0));
			pX[2] = X0;
			L0 = *pL; pL++;
			pX[3] = clampi16((int32_t)(X0 + L0) / 2);
		}

		pLowBand += nLowStep;
		pHighBand += nHighStep;
		pDstBand += nDstStep;
	}
}

static inline void progressive_rfx_idwt_y(const INT16* WINPR_RESTRICT pLowBand, size_t nLowStep,
                                          const INT16* WINPR_RESTRICT pHighBand, size_t nHighStep,
                                          INT16* WINPR_RESTRICT pDstBand, size_t nDstStep,
                                          size_t nLowCount, size_t nHighCount, size_t nDstCount)
{
	for (size_t i = 0; i < nDstCount; i++)
	{
		INT16 H1 = 0;
		INT16 X1 = 0;
		const INT16* pL = pLowBand;
		const INT16* pH = pHighBand;
		INT16* pX = pDstBand;
		INT16 H0 = *pH; pH += nHighStep;
		INT16 L0 = *pL; pL += nLowStep;
		int16_t X0 = clampi16((int32_t)L0 - H0);
		int16_t X2 = clampi16((int32_t)L0 - H0);

		for (size_t j = 0; j < (nHighCount - 1); j++)
		{
			H1 = *pH; pH += nHighStep;
			L0 = *pL; pL += nLowStep;
			X2 = clampi16((int32_t)L0 - ((H0 + H1) / 2));
			X1 = clampi16((int32_t)((X0 + X2) / 2) + (2 * H0));
			*pX = X0; pX += nDstStep;
			*pX = X1; pX += nDstStep;
			X0 = X2;
			H0 = H1;
		}

		if (nLowCount <= (nHighCount + 1))
		{
			if (nLowCount <= nHighCount)
			{
				*pX = X2; pX += nDstStep;
				*pX = clampi16((int32_t)X2 + (2 * H0));
			}
			else
			{
				L0 = *pL;
				X0 = clampi16((int32_t)L0 - H0);
				*pX = X2; pX += nDstStep;
				*pX = clampi16((int32_t)((X0 + X2) / 2) + (2 * H0));
				pX += nDstStep;
				*pX = X0;
			}
		}
		else
		{
			L0 = *pL; pL += nLowStep;
			X0 = clampi16((int32_t)L0 - (H0 / 2));
			*pX = X2; pX += nDstStep;
			*pX = clampi16((int32_t)((X0 + X2) / 2) + (2 * H0));
			pX += nDstStep;
			*pX = X0;
			pX += nDstStep;
			L0 = *pL;
			*pX = clampi16((int32_t)(X0 + L0) / 2);
		}

		pLowBand++;
		pHighBand++;
		pDstBand++;
	}
}

static inline size_t band_l(size_t level) { return (64 >> level) + 1; }
static inline size_t band_h(size_t level) {
	if (level == 1) return (64 >> 1) - 1;
	return (64 + (1u << (level - 1))) >> level;
}

static void decode_block(INT16* buffer, INT16* temp, size_t level)
{
	const size_t nBandL = band_l(level);
	const size_t nBandH = band_h(level);
	size_t offset = 0;
	const INT16* HL = &buffer[offset]; offset += nBandH * nBandL;
	const INT16* LH = &buffer[offset]; offset += nBandL * nBandH;
	const INT16* HH = &buffer[offset]; offset += nBandH * nBandH;
	INT16* LL = &buffer[offset];
	size_t nDstStepX = nBandL + nBandH;
	size_t nDstStepY = nBandL + nBandH;
	offset = 0;
	INT16* L = &temp[offset]; offset += nBandL * nDstStepX;
	INT16* H = &temp[offset];
	INT16* LLx = &buffer[0];
	progressive_rfx_idwt_x(LL, nBandL, HL, nBandH, L, nDstStepX, nBandL, nBandH, nBandL);
	progressive_rfx_idwt_x(LH, nBandL, HH, nBandH, H, nDstStepX, nBandL, nBandH, nBandH);
	progressive_rfx_idwt_y(L, nDstStepX, H, nDstStepX, LLx, nDstStepY, nBandL, nBandH, nBandL + nBandH);
}

void rfx_dwt_2d_extrapolate_decode(INT16* buffer, INT16* dwt_buffer)
{
	decode_block(&buffer[3807], dwt_buffer, 3);
	decode_block(&buffer[3007], dwt_buffer, 2);
	decode_block(&buffer[0], dwt_buffer, 1);
}


int main(int argc, char** argv){
    static INT16 buffer[4096]; static INT16 temp[4096];
    int idx = atoi(argv[1]); int val = atoi(argv[2]);
    memset(buffer,0,sizeof buffer); memset(temp,0,sizeof temp);
    buffer[idx] = (INT16)val;
    rfx_dwt_2d_extrapolate_decode(buffer, temp);
    for (int i=0;i<4096;i++) printf("%d\n", buffer[i]);
    return 0;
}

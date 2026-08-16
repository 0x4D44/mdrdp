/* Decode the captured RFX Progressive payloads with FreeRDP's own decoder,
 * so our output can be diffed against ground truth without any display. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <freerdp/codec/progressive.h>
#include <freerdp/codec/region.h>

int main(int argc, char** argv)
{
    const UINT32 W = 1920, H = 1080;
    const UINT32 fmt = PIXEL_FORMAT_BGRX32;
    const UINT32 step = W * 4;
    BYTE* dst = calloc((size_t)step * H, 1);
    PROGRESSIVE_CONTEXT* ctx = progressive_context_new(FALSE);
    if (!ctx || !dst) { fprintf(stderr, "alloc failed\n"); return 1; }
    if (progressive_create_surface_context(ctx, 0, W, H) < 0) {
        fprintf(stderr, "surface context failed\n"); return 1;
    }

    for (int i = 1; i < argc; i++) {
        FILE* f = fopen(argv[i], "rb");
        if (!f) { fprintf(stderr, "open %s failed\n", argv[i]); continue; }
        fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
        BYTE* buf = malloc((size_t)n);
        if (fread(buf, 1, (size_t)n, f) != (size_t)n) { fclose(f); free(buf); continue; }
        fclose(f);

        REGION16 invalid;
        region16_init(&invalid);
        INT32 rc = progressive_decompress(ctx, buf, (UINT32)n, dst, fmt, step, 0, 0,
                                          &invalid, 0, (UINT32)i);
        printf("  %s -> rc=%d\n", argv[i], rc);
        region16_uninit(&invalid);
        free(buf);
    }

    /* Dump the tile means FreeRDP produces for row 0, so they can be compared
     * against ours directly. */
    printf("FreeRDP row-0 tile means (B,G,R):\n");
    for (int tx = 0; tx < 30; tx++) {
        unsigned long b = 0, g = 0, r = 0, cnt = 0;
        for (UINT32 y = 4; y < 64; y += 6)
            for (UINT32 x = (UINT32)tx * 64 + 4; x < (UINT32)tx * 64 + 64; x += 6) {
                BYTE* p = dst + (size_t)y * step + (size_t)x * 4;
                b += p[0]; g += p[1]; r += p[2]; cnt++;
            }
        printf("   tile %2d rgb(%lu, %lu, %lu)\n", tx, r / cnt, g / cnt, b / cnt);
    }
    progressive_context_free(ctx);
    free(dst);
    return 0;
}

#include "hisi_raw.h"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

typedef struct { unsigned int frames, changed; uint32_t last; } stats;
static int on_frame(const hisi_raw_frame *f, void *ctx) {
    stats *s = ctx; uint32_t hash = 2166136261u;
    if (f->width != 1280 || f->height != 720 || f->stride_y != 1280 || f->stride_vu != 1280) return -1;
    for (size_t i = 0; i < (size_t)f->stride_y * f->height; i += 257) hash = (hash ^ f->y[i]) * 16777619u;
    for (size_t i = 0; i < (size_t)f->stride_vu * (f->height / 2); i += 257) hash = (hash ^ f->vu[i]) * 16777619u;
    if (s->frames && s->last != hash) ++s->changed;
    s->last = hash; ++s->frames; return 0;
}
static int run_once(FILE *file, int attempt) {
    hisi_raw_decoder *d = NULL; unsigned char data[4096]; size_t n;
    stats st = {0}; int rc = 1;
    if (hisi_raw_open(&d)) goto done;
    while ((n = fread(data, 1, sizeof data, file))) {
        int result, retries = 0;
        do {
            result = hisi_raw_submit(d, data, n);
            if (result == 1) { if (hisi_raw_poll(d, on_frame, &st) < 0) goto done; usleep(10000); }
        } while (result == 1 && ++retries < 100);
        if (result != 0) goto done;
        usleep(30000);
        if (hisi_raw_poll(d, on_frame, &st) < 0) goto done;
        if (st.frames >= 40) break;
    }
    rc = st.frames >= 20 && st.changed > 0 ? 0 : 1;
done:
    fprintf(stderr, "run %d: validated frames=%u distinct transitions=%u\n", attempt, st.frames, st.changed);
    hisi_raw_close(d); return rc;
}
int main(int argc, char **argv) {
    FILE *file;
    if (argc != 2 || !(file = fopen(argv[1], "rb"))) return 2;
    if (hisi_raw_open(NULL) != -1 || hisi_raw_submit(NULL, NULL, 0) != -1 || hisi_raw_poll(NULL, NULL, NULL) != -1) {
        fclose(file); return 1;
    }
    int rc = run_once(file, 1);
    if (!rc && !fseek(file, 0, SEEK_SET)) rc = run_once(file, 2);
    else rc = 1;
    fclose(file); return rc;
}

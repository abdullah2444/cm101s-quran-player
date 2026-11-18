/* Experimental single-threaded 720p H.264 decoder backend. Not a Linux driver. */
#ifndef HISI_RAW_H
#define HISI_RAW_H
#include <stddef.h>
#include <stdint.h>

typedef struct hisi_raw_decoder hisi_raw_decoder;
typedef struct {
    const uint8_t *y;
    const uint8_t *vu;
    uint32_t width, height, stride_y, stride_vu;
} hisi_raw_frame;
/* Frame pointers are valid only during the callback. NV21 chroma is V,U. */
typedef int (*hisi_raw_frame_cb)(const hisi_raw_frame *, void *);

/* The experimental ABI is only verified for this box and 1280x720 H.264. */
int hisi_raw_open(hisi_raw_decoder **out);
/* Returns 0, 1 if hardware input buffer is temporarily full, or -1 on error. */
int hisi_raw_submit(hisi_raw_decoder *, const uint8_t *bytes, size_t size);
/* Returns number of frames released, or -1 on error. Callback must not retain buffers. */
int hisi_raw_poll(hisi_raw_decoder *, hisi_raw_frame_cb, void *opaque);
void hisi_raw_close(hisi_raw_decoder *);
#endif

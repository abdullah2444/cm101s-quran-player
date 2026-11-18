/* Isolated frame-returning decoder prototype for CM101s-2 firmware.
 * This module is not installed as a standard driver. */
#include "hisi_raw.h"
#include <stdlib.h>
#include <string.h>
#include <stddef.h>
#include "hi_common.h"
#include "hi_mpi_vdec.h"

extern HI_VOID *HI_MPI_MMZ_Map(HI_U32, HI_BOOL);
extern HI_S32 HI_MPI_MMZ_Unmap(HI_U32);
extern HI_S32 HI_MPI_MMZ_GetPhyAddr(HI_VOID *, HI_U32 *, HI_U32 *);

struct hisi_raw_decoder {
    HI_HANDLE dec, port;
    int sys, init, allocated, buffered, port_created, enabled, started;
};

void hisi_raw_close(hisi_raw_decoder *s) {
    if (!s) return;
    if (s->started) HI_MPI_VDEC_ChanStop(s->dec);
    if (s->enabled) HI_MPI_VDEC_DisablePort(s->dec, s->port);
    if (s->port_created) HI_MPI_VDEC_DestroyPort(s->dec, s->port);
    if (s->buffered) HI_MPI_VDEC_ChanBufferDeInit(s->dec);
    if (s->allocated) HI_MPI_VDEC_FreeChan(s->dec);
    if (s->init) HI_MPI_VDEC_DeInit();
    if (s->sys) HI_SYS_DeInit();
    free(s);
}

int hisi_raw_open(hisi_raw_decoder **out) {
    HI_UNF_AVPLAY_OPEN_OPT_S opt; HI_UNF_VCODEC_ATTR_S attr; HI_DRV_VPSS_PORT_CFG_S cfg;
    hisi_raw_decoder *s;
    if (!out) return -1;
    *out = NULL;
    if (!(s = calloc(1, sizeof *s))) return -1;
    s->dec = s->port = HI_INVALID_HANDLE;
    if (HI_SYS_Init() != HI_SUCCESS) goto fail;
    s->sys = 1;
    if (HI_MPI_VDEC_Init() != HI_SUCCESS) goto fail;
    s->init = 1;
    memset(&opt, 0, sizeof opt); opt.enDecType = HI_UNF_VCODEC_DEC_TYPE_NORMAL;
    opt.enCapLevel = HI_UNF_VCODEC_CAP_LEVEL_720P; opt.enProtocolLevel = HI_UNF_VCODEC_PRTCL_LEVEL_H264;
    if (HI_MPI_VDEC_AllocChan(&s->dec, &opt) != HI_SUCCESS) goto fail;
    s->allocated = 1;
    if (HI_MPI_VDEC_GetChanAttr(s->dec, &attr) != HI_SUCCESS) goto fail;
    attr.enType = HI_UNF_VCODEC_TYPE_H264; attr.enMode = HI_UNF_VCODEC_MODE_NORMAL; attr.u32ErrCover = 100; attr.u32Priority = 3;
    if (HI_MPI_VDEC_SetChanAttr(s->dec, &attr) != HI_SUCCESS) goto fail;
    if (HI_MPI_VDEC_ChanBufferInit(s->dec, 4 * 1024 * 1024, HI_INVALID_HANDLE) != HI_SUCCESS) goto fail;
    s->buffered = 1;
    if (HI_MPI_VDEC_CreatePort(s->dec, &s->port, VDEC_PORT_HD) != HI_SUCCESS) goto fail;
    s->port_created = 1;
    if (HI_MPI_VDEC_SetPortType(s->dec, s->port, VDEC_PORT_TYPE_MASTER) != HI_SUCCESS) goto fail;
    if (HI_MPI_VDEC_GetPortAttr(s->dec, s->port, &cfg) != HI_SUCCESS) goto fail;
    cfg.s32OutputWidth = 1280; cfg.s32OutputHeight = 720; cfg.eFormat = HI_DRV_PIX_FMT_NV21;
    if (HI_MPI_VDEC_SetPortAttr(s->dec, s->port, &cfg) != HI_SUCCESS) goto fail;
    if (HI_MPI_VDEC_EnablePort(s->dec, s->port) != HI_SUCCESS) goto fail;
    s->enabled = 1;
    if (HI_MPI_VDEC_ChanStart(s->dec) != HI_SUCCESS) goto fail;
    s->started = 1; *out = s; return 0;
fail:
    hisi_raw_close(s); return -1;
}

int hisi_raw_submit(hisi_raw_decoder *s, const uint8_t *bytes, size_t size) {
    VDEC_ES_BUF_S b;
    if (!s || !s->started || !bytes || !size || size > 1024 * 1024) return -1;
    memset(&b, 0, sizeof b);
    if (HI_MPI_VDEC_ChanGetBuffer(s->dec, (HI_U32)size, &b) != HI_SUCCESS) return 1;
    memcpy(b.pu8Addr, bytes, size); b.u32BufSize = (HI_U32)size; b.u64Pts = 0xffffffff;
    return HI_MPI_VDEC_ChanPutBuffer(s->dec, &b) == HI_SUCCESS ? 0 : -1;
}

int hisi_raw_poll(hisi_raw_decoder *s, hisi_raw_frame_cb callback, void *opaque) {
    HI_DRV_VIDEO_FRAME_PACKAGE_S pack; int count = 0, error = 0;
    if (!s || !s->started || !callback) return -1;
    memset(&pack, 0, sizeof pack);
    if (HI_MPI_VDEC_ReceiveFrame(s->dec, &pack, 0) != HI_SUCCESS) return 0;
    if (!pack.u32FrmNum || pack.u32FrmNum > DEF_HI_DRV_FRAME_PACKAGE_MAX_FRAME_NUMBER) return -1;
    for (unsigned int i = 0; i < pack.u32FrmNum; ++i) {
        HI_DRV_VIDEO_FRAME_S *v = &pack.stFrame[i].stFrameVideo;
        HI_U32 y = v->stBufAddr[0].u32PhyAddr_Y, c = v->stBufAddr[0].u32PhyAddr_C, mapped_addr = 0, mapped_size = 0;
        uint32_t dims[3]; unsigned char *map = NULL;
        memcpy(dims, (unsigned char *)v + offsetof(HI_DRV_VIDEO_FRAME_S, u32Width), sizeof dims);
        if (pack.stFrame[i].hport == s->port && dims[1] == 1280 && dims[2] == 720 && y && c >= y &&
            v->stBufAddr[0].u32Stride_Y == 1280 && v->stBufAddr[0].u32Stride_C == 1280)
            map = HI_MPI_MMZ_Map(y, HI_FALSE);
        if (!map || HI_MPI_MMZ_GetPhyAddr(map, &mapped_addr, &mapped_size) != HI_SUCCESS || mapped_addr != y ||
            mapped_size < 1280 * 720 || c - y > mapped_size || mapped_size - (c - y) < 1280 * 360) {
            error = 1;
        } else {
            hisi_raw_frame frame = { map, map + (c - y), 1280, 720, 1280, 1280 };
            if (callback(&frame, opaque)) error = 1;
        }
        if (map && HI_MPI_MMZ_Unmap(y) != HI_SUCCESS) error = 1;
        if (pack.stFrame[i].hport != s->port || HI_MPI_VDEC_ReleaseFrame(s->port, v) != HI_SUCCESS) error = 1;
        else ++count;
    }
    return error ? -1 : count;
}

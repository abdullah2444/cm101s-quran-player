/* Isolated headless decoder/VPSS probe. Experimental; not a system driver. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <stddef.h>
#include "hi_common.h"
#include "hi_mpi_vdec.h"
extern HI_VOID *HI_MPI_MMZ_Map(HI_U32, HI_BOOL);
extern HI_S32 HI_MPI_MMZ_Unmap(HI_U32);
extern HI_S32 HI_MPI_MMZ_GetPhyAddr(HI_VOID *, HI_U32 *, HI_U32 *);

int main(int argc, char **argv) {
    FILE *f; unsigned char data[4096]; size_t n; HI_S32 e; HI_HANDLE dec = HI_INVALID_HANDLE, port = HI_INVALID_HANDLE;
    HI_UNF_AVPLAY_OPEN_OPT_S opt; HI_UNF_VCODEC_ATTR_S attr; VDEC_ES_BUF_S buf;
    HI_DRV_VIDEO_FRAME_PACKAGE_S pack; HI_DRV_VPSS_PORT_CFG_S portcfg;
    int sys = 0, init = 0, alloc = 0, buffered = 0, created = 0, enabled = 0, started = 0;
    unsigned int submitted = 0, received = 0, valid = 0; int rc = 1;
    if (argc != 2 || !(f = fopen(argv[1], "rb"))) return 2;
#define CHECK(call) do { e = (call); fprintf(stderr, "%s => 0x%08x\n", #call, (unsigned)e); if (e != HI_SUCCESS) goto done; } while (0)
    CHECK(HI_SYS_Init()); sys = 1;
    CHECK(HI_MPI_VDEC_Init()); init = 1;
    memset(&opt, 0, sizeof opt); opt.enDecType = HI_UNF_VCODEC_DEC_TYPE_NORMAL;
    opt.enCapLevel = HI_UNF_VCODEC_CAP_LEVEL_720P; opt.enProtocolLevel = HI_UNF_VCODEC_PRTCL_LEVEL_H264;
    CHECK(HI_MPI_VDEC_AllocChan(&dec, &opt)); alloc = 1;
    CHECK(HI_MPI_VDEC_GetChanAttr(dec, &attr));
    attr.enType = HI_UNF_VCODEC_TYPE_H264; attr.enMode = HI_UNF_VCODEC_MODE_NORMAL; attr.u32ErrCover = 100; attr.u32Priority = 3;
    CHECK(HI_MPI_VDEC_SetChanAttr(dec, &attr));
    CHECK(HI_MPI_VDEC_ChanBufferInit(dec, 4 * 1024 * 1024, HI_INVALID_HANDLE)); buffered = 1;
    CHECK(HI_MPI_VDEC_CreatePort(dec, &port, VDEC_PORT_HD)); created = 1;
    CHECK(HI_MPI_VDEC_SetPortType(dec, port, VDEC_PORT_TYPE_MASTER));
    CHECK(HI_MPI_VDEC_GetPortAttr(dec, port, &portcfg));
    fprintf(stderr, "port default %dx%d format=%d\n", portcfg.s32OutputWidth, portcfg.s32OutputHeight, portcfg.eFormat);
    portcfg.s32OutputWidth = 1280; portcfg.s32OutputHeight = 720; portcfg.eFormat = HI_DRV_PIX_FMT_NV21;
    CHECK(HI_MPI_VDEC_SetPortAttr(dec, port, &portcfg));
    CHECK(HI_MPI_VDEC_EnablePort(dec, port)); enabled = 1;
    CHECK(HI_MPI_VDEC_ChanStart(dec)); started = 1;
    while ((n = fread(data, 1, sizeof data, f)) != 0) {
        memset(&buf, 0, sizeof buf);
        for (int tries = 0; tries < 50; ++tries) {
            e = HI_MPI_VDEC_ChanGetBuffer(dec, n, &buf);
            if (e == HI_SUCCESS) break;
            usleep(10000);
        }
        if (e != HI_SUCCESS) { fprintf(stderr, "GetBuffer failed 0x%08x\n", (unsigned)e); break; }
        memcpy(buf.pu8Addr, data, n); buf.u32BufSize = n; buf.u64Pts = 0xffffffff;
        if ((e = HI_MPI_VDEC_ChanPutBuffer(dec, &buf)) != HI_SUCCESS) { fprintf(stderr, "PutBuffer failed 0x%08x\n", (unsigned)e); break; }
        ++submitted; usleep(30000);
        memset(&pack, 0, sizeof pack);
        if (HI_MPI_VDEC_ReceiveFrame(dec, &pack, 0) == HI_SUCCESS) {
            fprintf(stderr, "received package frames=%u\n", pack.u32FrmNum);
            if (pack.u32FrmNum > 0 && pack.u32FrmNum <= DEF_HI_DRV_FRAME_PACKAGE_MAX_FRAME_NUMBER) {
                for (unsigned int i = 0; i < pack.u32FrmNum; ++i) {
                    HI_DRV_VIDEO_FRAME_S *v = &pack.stFrame[i].stFrameVideo;
                    uint32_t words[4];
                    memcpy(words, (unsigned char *)v + offsetof(HI_DRV_VIDEO_FRAME_S, u32Width), sizeof words);
                    if (words[1] == 1280 && words[2] == 720 && v->stBufAddr[0].u32Stride_Y == 1280 && v->stBufAddr[0].u32PhyAddr_Y) ++valid;
                    if (!received) fprintf(stderr, "first frame raw dimensions=%u,%u,%u strideY=%u strideC=%u planes=%d/%d port_matches=%d\n",
                        words[0], words[1], words[2], v->stBufAddr[0].u32Stride_Y, v->stBufAddr[0].u32Stride_C,
                        v->stBufAddr[0].u32PhyAddr_Y != 0, v->stBufAddr[0].u32PhyAddr_C != 0, pack.stFrame[i].hport == port);
                    if (!received && words[1] == 1280 && words[2] == 720 && v->stBufAddr[0].u32Stride_Y == 1280 && v->stBufAddr[0].u32PhyAddr_Y) {
                        HI_U32 phy = 0, size = 0, addr = v->stBufAddr[0].u32PhyAddr_Y;
                        const unsigned char *pixels = HI_MPI_MMZ_Map(addr, HI_FALSE);
                        if (pixels) {
                            e = HI_MPI_MMZ_GetPhyAddr((HI_VOID *)pixels, &phy, &size);
                            fprintf(stderr, "map: present=1 query=0x%08x matches=%d bytes=%u\n", (unsigned)e, phy == addr, size);
                            if (e == HI_SUCCESS && phy == addr && size >= 1280 * 720) {
                                FILE *out = fopen("decoded-luma.pgm", "wb");
                                if (out) { fprintf(out, "P5\n1280 720\n255\n"); fwrite(pixels, 1280, 720, out); fclose(out); }
                                HI_U32 chroma = v->stBufAddr[0].u32PhyAddr_C;
                                if (chroma >= addr && chroma - addr <= size && size - (chroma - addr) >= 1280 * 360 && v->stBufAddr[0].u32Stride_C == 1280) {
                                    out = fopen("decoded-frame.nv21", "wb");
                                    if (out) { fwrite(pixels, 1280, 720, out); fwrite(pixels + (chroma - addr), 1280, 360, out); fclose(out); }
                                    fprintf(stderr, "chroma: within verified allocation, offset=%u\n", chroma - addr);
                                } else fprintf(stderr, "chroma: cannot validate bounds\n");
                            }
                            HI_MPI_MMZ_Unmap(addr);
                        } else fprintf(stderr, "map: unavailable\n");
                    }
                    if (pack.stFrame[i].hport == port) {
                        e = HI_MPI_VDEC_ReleaseFrame(port, v);
                        if (e == HI_SUCCESS) ++received; else fprintf(stderr, "release failed => 0x%08x\n", (unsigned)e);
                    }
                }
            }
            if (received >= 40) break;
        }
    }
    fprintf(stderr, "submitted=%u valid=%u released=%u\n", submitted, valid, received);
    rc = (received >= 20 && valid == received) ? 0 : 3;
done:
    if (started) HI_MPI_VDEC_ChanStop(dec);
    if (enabled) HI_MPI_VDEC_DisablePort(dec, port);
    if (created) HI_MPI_VDEC_DestroyPort(dec, port);
    if (buffered) HI_MPI_VDEC_ChanBufferDeInit(dec);
    if (alloc) HI_MPI_VDEC_FreeChan(dec);
    if (init) HI_MPI_VDEC_DeInit();
    if (sys) HI_SYS_DeInit();
    fclose(f); return rc;
}

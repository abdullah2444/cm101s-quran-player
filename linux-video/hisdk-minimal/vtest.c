/* vtest: play a raw H.265/H.264 elementary stream through the Hi3798 hardware decoder
 * onto the video layer.  usage: vtest file.h265 [h264] [x y w h] [seconds] */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <time.h>
#include "hi_common.h"
#include "hi_unf_avplay.h"
#include "hi_unf_vo.h"
#include "hi_unf_disp.h"
extern HI_VOID *HI_MPI_MMZ_Map(HI_U32 u32PhysAddr, HI_BOOL bCached);
extern HI_VOID *HI_MMAP(HI_U32 phy_addr, HI_U32 size);

#define CK(x) do { HI_S32 _r = (x); printf("%-60s -> 0x%08x\n", #x, _r); fflush(stdout); \
                   if (_r != HI_SUCCESS) goto out; } while (0)

static volatile int stop;
static void onsig(int s) { stop = 1; }

int main(int argc, char **argv)
{
    HI_HANDLE hAv = 0, hWin = 0;
    HI_UNF_AVPLAY_ATTR_S attr;
    HI_UNF_SYNC_ATTR_S sync;
    HI_UNF_VCODEC_ATTR_S vdec;
    HI_UNF_WINDOW_ATTR_S win;
    HI_UNF_STREAM_BUF_S buf;
    int h264 = argc > 2 && !strcmp(argv[2], "h264");
    int a = h264 ? 3 : 2;
    int secs = 20;
    FILE *fp;
    if (argc < 2) { fprintf(stderr, "usage\n"); return 1; }
    fp = fopen(argv[1], "rb");
    if (!fp) { perror("open"); return 1; }
    signal(SIGINT, onsig); signal(SIGTERM, onsig);

    memset(&win, 0, sizeof win);
    win.enDisp = HI_UNF_DISPLAY1;
    win.stWinAspectAttr.enAspectCvrs = HI_UNF_VO_ASPECT_CVRS_IGNORE;
    if (argc >= a + 4) {
        win.stOutputRect.s32X = atoi(argv[a]); win.stOutputRect.s32Y = atoi(argv[a+1]);
        win.stOutputRect.s32Width = atoi(argv[a+2]); win.stOutputRect.s32Height = atoi(argv[a+3]);
        a += 4;
    }
    if (argc > a) secs = atoi(argv[a]);

    CK(HI_SYS_Init());
    CK(HI_UNF_DISP_Init());
    CK(HI_UNF_VO_Init(HI_UNF_VO_DEV_MODE_NORMAL));
    CK(HI_UNF_VO_CreateWindow(&win, &hWin));
    CK(HI_UNF_AVPLAY_Init());
    CK(HI_UNF_AVPLAY_GetDefaultConfig(&attr, HI_UNF_AVPLAY_STREAM_TYPE_ES));
    attr.stStreamAttr.u32VidBufSize = 2 * 1024 * 1024;
    CK(HI_UNF_AVPLAY_Create(&attr, &hAv));
    CK(HI_UNF_AVPLAY_GetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_SYNC, &sync));
    sync.enSyncRef = HI_UNF_SYNC_REF_NONE;
    CK(HI_UNF_AVPLAY_SetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_SYNC, &sync));
    CK(HI_UNF_AVPLAY_ChnOpen(hAv, HI_UNF_AVPLAY_MEDIA_CHAN_VID, HI_NULL));
    CK(HI_UNF_AVPLAY_GetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_VDEC, &vdec));
    vdec.enType = h264 ? HI_UNF_VCODEC_TYPE_H264 : HI_UNF_VCODEC_TYPE_HEVC;
    vdec.enMode = HI_UNF_VCODEC_MODE_NORMAL;
    vdec.u32ErrCover = 100;
    vdec.u32Priority = 3;
    CK(HI_UNF_AVPLAY_SetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_VDEC, &vdec));
    CK(HI_UNF_VO_AttachWindow(hWin, hAv));
    CK(HI_UNF_VO_SetWindowEnable(hWin, HI_TRUE));
    CK(HI_UNF_AVPLAY_Start(hAv, HI_UNF_AVPLAY_MEDIA_CHAN_VID, HI_NULL));

    {
        time_t t0 = time(NULL);
        long fed = 0;
        int captured = 0;
        while (!stop && time(NULL) - t0 < secs) {
            if (HI_UNF_AVPLAY_GetBuf(hAv, HI_UNF_AVPLAY_BUF_ID_ES_VID, 0x4000, &buf, 0) != HI_SUCCESS) {
                usleep(10000); continue;
            }
            size_t n = fread(buf.pu8Data, 1, 0x4000, fp);
            if (n == 0) { rewind(fp); continue; }   /* loop the clip */
            HI_UNF_AVPLAY_PutBuf(hAv, HI_UNF_AVPLAY_BUF_ID_ES_VID, n, 0);
            fed += n;
            usleep(20000);
            if (!captured && time(NULL) - t0 >= 5) {      /* grab what the window shows, save Y as PGM */
                HI_UNF_VIDEO_FRAME_INFO_S cap;
                HI_S32 r;
                captured = 1;
                memset(&cap, 0, sizeof cap);
                r = HI_UNF_VO_CapturePicture(hWin, &cap);
                printf("capture -> 0x%08x  %ux%u  Y=0x%x stride %u fmt %d\n", r, cap.u32Width, cap.u32Height,
                       cap.stVideoFrameAddr[0].u32YAddr, cap.stVideoFrameAddr[0].u32YStride, cap.enVideoFormat);
                if (r == HI_SUCCESS && cap.stVideoFrameAddr[0].u32YAddr) {
                    HI_U32 sz = cap.stVideoFrameAddr[0].u32YStride * cap.u32Height;
                    HI_U8 *y = (HI_U8 *)HI_MPI_MMZ_Map(cap.stVideoFrameAddr[0].u32YAddr, HI_FALSE);
                    if (!y) y = (HI_U8 *)HI_MMAP(cap.stVideoFrameAddr[0].u32YAddr, sz);
                    printf("map %p\n", y);
                    if (y) {
                        FILE *o = fopen("/tmp/cap.pgm", "wb");
                        HI_U32 row;
                        fprintf(o, "P5 %u %u 255\n", cap.u32Width, cap.u32Height);
                        for (row = 0; row < cap.u32Height; row++)
                            fwrite(y + row * cap.stVideoFrameAddr[0].u32YStride, 1, cap.u32Width, o);
                        fclose(o);
                    }
                    HI_UNF_VO_CapturePictureRelease(hWin, &cap);
                }
                fflush(stdout);
            }
        }
        printf("fed %ld bytes in %ld s\n", fed, (long)(time(NULL) - t0));
    }

out:
    if (hAv) {
        HI_UNF_AVPLAY_STOP_OPT_S st = { HI_UNF_AVPLAY_STOP_MODE_BLACK, 0 };
        HI_UNF_AVPLAY_Stop(hAv, HI_UNF_AVPLAY_MEDIA_CHAN_VID, &st);
        if (hWin) { HI_UNF_VO_SetWindowEnable(hWin, HI_FALSE); HI_UNF_VO_DetachWindow(hWin, hAv); }
        HI_UNF_AVPLAY_ChnClose(hAv, HI_UNF_AVPLAY_MEDIA_CHAN_VID);
        HI_UNF_AVPLAY_Destroy(hAv);
    }
    if (hWin) HI_UNF_VO_DestroyWindow(hWin);
    HI_UNF_AVPLAY_DeInit();
    HI_UNF_VO_DeInit();
    HI_UNF_DISP_DeInit();
    HI_SYS_DeInit();
    return 0;
}

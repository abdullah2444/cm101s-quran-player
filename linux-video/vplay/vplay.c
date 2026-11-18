/* vplay - play a video file on the TV: picture through the Hi3798MV100 hardware decoder
 * (H.264 / H.265, on the video layer under the graphics layer), sound through ALSA (AUX).
 *
 * usage: vplay <file> [start seconds] [volume 0-99]
 * stdin commands (one per line):  p = pause/resume   s <sec> = seek   v <0-99> = volume   q = quit
 * stdout: "T <pos ms> <duration ms> <paused>" every 0.5 s, "E" at the end, "ERR <text>" on failure.
 * The audio clock is the master: video access units are handed to the decoder when their
 * decode time is due (clock + VPLAY_LEAD ms, default 120).  Volume: same dB scale as the
 * Quran player (99 = 0 dB, 0.5 dB per step, 0 = mute).
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <math.h>
#include <time.h>
#include <fcntl.h>
#include <pthread.h>
#include <alsa/asoundlib.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libswresample/swresample.h>
#include <libavutil/opt.h>
#include "hi_common.h"
#include "hi_unf_avplay.h"
#include "hi_unf_vo.h"
#include "hi_unf_disp.h"

extern HI_VOID *HI_MPI_MMZ_Map(HI_U32 u32PhysAddr, HI_BOOL bCached);
#define RATE 48000
#define AUDIO_DEV "plughw:0,0"

typedef struct AChunk { struct AChunk *next; int64_t pts; int n, off, raw; int16_t s[]; } AChunk;
typedef struct VPkt { struct VPkt *next; int64_t dts, pts; int len; uint8_t d[]; } VPkt;

static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t c = PTHREAD_COND_INITIALIZER;
static AChunk *aq, *aqt;              /* decoded audio waiting for ALSA */
static VPkt *vq, *vqt; static int vqn; /* video access units waiting for their time */
static volatile int quit, paused, seekgen, has_audio;
static volatile int64_t aclock = -1;  /* ms of audio being heard now (-1 = not started) */
static volatile float gain = 1.0f;
static snd_pcm_t *pcm;
static int64_t sys_base;              /* no-audio files: clock from the system clock */
static volatile int64_t ahold_until;  /* main loop: no new check until the inserted silence has played */
static volatile int asilence;         /* frames of silence to play before the next sound: lets late video catch up */
static int hwpause = -1;              /* ALSA device can pause in place (-1 = not tried yet) */
#define HIST (RATE / 2)                 /* last 0.5 s written to ALSA: replayed after a pause drop */
static int16_t hist[HIST * 2]; static int histpos;
static void hist_add(const int16_t *x, int n)
{
    for (int i = 0; i < n; i++) { hist[histpos * 2] = x[i * 2]; hist[histpos * 2 + 1] = x[i * 2 + 1]; histpos = (histpos + 1) % HIST; }
}

static int64_t now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1000LL + t.tv_nsec / 1000000; }
static void onsig(int s) { quit = 1; pthread_cond_broadcast(&c); }

static void set_volume(int v)
{
    if (v < 0) v = 0; if (v > 99) v = 99;
    gain = v == 0 ? 0.0f : powf(10.0f, (v - 99) * 0.5f / 20.0f);
}

static int64_t clock_now(void)
{
    if (!has_audio) return paused ? sys_base : now_ms() - sys_base;
    if (aclock >= 0) return aclock;
    pthread_mutex_lock(&m);
    int64_t t = aq ? aq->pts : (vq ? vq->dts : 0);   /* not started yet: about to start here */
    pthread_mutex_unlock(&m);
    return t;
}

/* ---------------- audio output thread ---------------- */
static void *audio_thread(void *arg)
{
    int16_t tmp[2048 * 2];
    int gen = -1;
    while (1) {
        pthread_mutex_lock(&m);
        while (!quit && (paused || !aq)) {
            if (paused && pcm && snd_pcm_state(pcm) == SND_PCM_STATE_RUNNING) {
                pthread_mutex_unlock(&m);
                /* pause in place so the sound already in the buffer is not lost */
                if (hwpause != 0 && snd_pcm_pause(pcm, 1) == 0) { hwpause = 1; pthread_mutex_lock(&m); continue; }
                hwpause = 0;
                snd_pcm_sframes_t d = 0;
                if (snd_pcm_delay(pcm, &d) < 0 || d < 0) d = 0;
                if (d > HIST) d = HIST;
                snd_pcm_drop(pcm); snd_pcm_prepare(pcm);
                pthread_mutex_lock(&m);
                if (d > 0 && aclock >= 0) {          /* not heard yet: put it back in front of the queue */
                    AChunk *a = malloc(sizeof(AChunk) + d * 4);
                    int st = (histpos - (int)d + HIST) % HIST;
                    for (int i = 0; i < d; i++) { int k = (st + i) % HIST; a->s[i * 2] = hist[k * 2]; a->s[i * 2 + 1] = hist[k * 2 + 1]; }
                    a->n = d; a->off = 0; a->pts = aclock; a->next = aq; aq = a; if (!aqt) aqt = a;
                    a->raw = 1;
                }
                continue;
            }
            struct timespec ts; clock_gettime(CLOCK_REALTIME, &ts); ts.tv_nsec += 50000000;
            if (ts.tv_nsec >= 1000000000) { ts.tv_sec++; ts.tv_nsec -= 1000000000; }
            pthread_cond_timedwait(&c, &m, &ts);
        }
        if (quit) { pthread_mutex_unlock(&m); break; }
        if (pcm && snd_pcm_state(pcm) == SND_PCM_STATE_PAUSED) {
            pthread_mutex_unlock(&m); snd_pcm_pause(pcm, 0); continue;
        }
        if (gen != seekgen) {                       /* after a seek: throw away what ALSA holds */
            gen = seekgen;
            pthread_mutex_unlock(&m); snd_pcm_drop(pcm); snd_pcm_prepare(pcm);
            continue;
        }
        if (asilence > 0) {                     /* delay the sound by exactly this much */
            int z = asilence > 2048 ? 2048 : asilence;
            asilence -= z;
            pthread_mutex_unlock(&m);
            memset(tmp, 0, z * 4);
            snd_pcm_sframes_t r = snd_pcm_writei(pcm, tmp, z);
            if (r < 0) snd_pcm_recover(pcm, r, 1);
            continue;
        }
        AChunk *ch = aq;
        int n = ch->n - ch->off; if (n > 2048) n = 2048;
        float g = gain;
        if (ch->raw) memcpy(tmp, ch->s + ch->off * 2, n * 4);
        else for (int i = 0; i < n * 2; i++) tmp[i] = (int16_t)(ch->s[ch->off * 2 + i] * g);
        pthread_mutex_unlock(&m);
        snd_pcm_sframes_t r = snd_pcm_writei(pcm, tmp, n);
        if (r < 0) r = snd_pcm_recover(pcm, r, 1);
        if (r < 0) { usleep(10000); continue; }
        hist_add(tmp, (int)r);
        pthread_mutex_lock(&m);
        if (gen == seekgen && aq == ch) {
            ch->off += r;
            int64_t end = ch->pts + (int64_t)ch->off * 1000 / RATE;
            if (ch->off >= ch->n) { aq = ch->next; if (!aq) aqt = NULL; free(ch); }
            snd_pcm_sframes_t d = 0;
            if (snd_pcm_delay(pcm, &d) < 0) d = 0;
            aclock = end - (int64_t)d * 1000 / RATE;
        }
        pthread_mutex_unlock(&m);
    }
    return NULL;
}

static int audio_open(void)
{
    if (snd_pcm_open(&pcm, getenv("VPLAY_ADEV") ? getenv("VPLAY_ADEV") : AUDIO_DEV, SND_PCM_STREAM_PLAYBACK, 0) < 0) return -1;
    if (snd_pcm_set_params(pcm, SND_PCM_FORMAT_S16_LE, SND_PCM_ACCESS_RW_INTERLEAVED, 2, RATE, 1, 300000) < 0) return -1;
    return 0;
}

/* ---------------- hardware video ---------------- */
static HI_HANDLE hAv, hWin;
static int hi_up;
static HI_S32 on_event(HI_HANDLE h, HI_UNF_AVPLAY_EVENT_E ev, HI_VOID *para);
static int vadj;                         /* current s32VidPtsAdjust */

static void set_vadj(int a)
{
    HI_UNF_SYNC_ATTR_S sync;
    if (HI_UNF_AVPLAY_GetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_SYNC, &sync)) return;
    sync.s32VidPtsAdjust = a;
    if (HI_UNF_AVPLAY_SetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_SYNC, &sync) == HI_SUCCESS) vadj = a;
}

static int video_open(int hevc, int w, int h, int64_t fdur_ms)
{
    HI_UNF_AVPLAY_ATTR_S attr; HI_UNF_SYNC_ATTR_S sync; HI_UNF_VCODEC_ATTR_S vdec; HI_UNF_WINDOW_ATTR_S win;
    HI_UNF_AVPLAY_OPEN_OPT_S opt;
    if (HI_SYS_Init() || HI_UNF_DISP_Init() || HI_UNF_VO_Init(HI_UNF_VO_DEV_MODE_NORMAL) || HI_UNF_AVPLAY_Init()) return -1;
    hi_up = 1;
    /* note: the SD (CVBS) mirror must run PAL for 25 fps video, or the VPSS waits on the SD window
     * and the picture drops to ~15 fps.  sd-pal.service sets it once at boot (changing it here,
     * while the video path is starting, leaves the SD window stuck). */
    memset(&win, 0, sizeof win);
    win.enDisp = HI_UNF_DISPLAY1;
    win.stWinAspectAttr.enAspectCvrs = HI_UNF_VO_ASPECT_CVRS_LETTERBOX;
    win.stOutputRect.s32X = 0; win.stOutputRect.s32Y = 0;
    win.stOutputRect.s32Width = getenv("VPLAY_W") ? atoi(getenv("VPLAY_W")) : 1920;
    win.stOutputRect.s32Height = getenv("VPLAY_H") ? atoi(getenv("VPLAY_H")) : 1080;
    win.stOutputRect.s32X = (1920 - win.stOutputRect.s32Width) / 2;
    win.stOutputRect.s32Y = (1080 - win.stOutputRect.s32Height) / 2;
    if (HI_UNF_VO_CreateWindow(&win, &hWin)) { hWin = 0; return -2; }
    HI_UNF_AVPLAY_GetDefaultConfig(&attr, HI_UNF_AVPLAY_STREAM_TYPE_ES);
    attr.stStreamAttr.u32VidBufSize = 4 * 1024 * 1024;
    if (HI_UNF_AVPLAY_Create(&attr, &hAv)) { hAv = 0; return -3; }
    HI_UNF_AVPLAY_GetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_SYNC, &sync);
    /* the hardware shows each frame at its PTS against its own clock (SCR); the main loop keeps
     * nudging s32VidPtsAdjust so the shown frame matches the sound being heard */
    sync.enSyncRef = getenv("VPLAY_SYNC") ? (HI_UNF_SYNC_REF_E)atoi(getenv("VPLAY_SYNC")) : HI_UNF_SYNC_REF_SCR;
    sync.s32VidPtsAdjust = 0;
    HI_UNF_AVPLAY_SetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_SYNC, &sync);
    memset(&opt, 0, sizeof opt);
    opt.enDecType = HI_UNF_VCODEC_DEC_TYPE_NORMAL;
    opt.enCapLevel = (w > 1280 || h > 720 || getenv("VPLAY_FHD")) ? HI_UNF_VCODEC_CAP_LEVEL_FULLHD : HI_UNF_VCODEC_CAP_LEVEL_720P;
    opt.enProtocolLevel = HI_UNF_VCODEC_PRTCL_LEVEL_H264;
    if (HI_UNF_AVPLAY_ChnOpen(hAv, HI_UNF_AVPLAY_MEDIA_CHAN_VID, &opt)) return -4;
    HI_UNF_AVPLAY_GetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_VDEC, &vdec);
    vdec.enType = hevc ? HI_UNF_VCODEC_TYPE_HEVC : HI_UNF_VCODEC_TYPE_H264;
    vdec.enMode = HI_UNF_VCODEC_MODE_NORMAL;
    vdec.u32ErrCover = 100;
    vdec.u32Priority = 3;
    if (HI_UNF_AVPLAY_SetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_VDEC, &vdec)) return -5;
    if (HI_UNF_VO_AttachWindow(hWin, hAv)) return -6;
    if (HI_UNF_VO_SetWindowEnable(hWin, HI_TRUE)) return -7;
    HI_S32 er = HI_UNF_AVPLAY_RegisterEvent64(hAv, HI_UNF_AVPLAY_EVENT_NEW_VID_FRAME, on_event);
    if (getenv("VPLAY_DBG")) fprintf(stderr, "event reg %x\n", er);
    if (fdur_ms > 0 && !getenv("VPLAY_NOFPS")) {     /* tell the window the real frame rate (else it guesses) */
        HI_UNF_AVPLAY_FRMRATE_PARAM_S fp; memset(&fp, 0, sizeof fp);
        fp.enFrmRateType = HI_UNF_AVPLAY_FRMRATE_TYPE_USER;
        /* declare the display refresh (1080p60), not the file rate: then the window shows each
         * frame as soon as it is ready and repeats it until the next, and nothing in the VO/VPSS
         * frame-rate conversion holds the decoder back (with 25 declared it sank to ~14 fps).
         * Real timing comes from how far ahead of the sound we feed the decoder ("lead"). */
        fp.stSetFrmRate.u32fpsInteger = 60;
        fp.stSetFrmRate.u32fpsDecimal = 0;
        if (getenv("VPLAY_FPS")) { fp.stSetFrmRate.u32fpsInteger = atoi(getenv("VPLAY_FPS")); fp.stSetFrmRate.u32fpsDecimal = 0; }
        if (getenv("VPLAY_FPSTYPE")) fp.enFrmRateType = atoi(getenv("VPLAY_FPSTYPE"));
        HI_S32 r = HI_UNF_AVPLAY_SetAttr(hAv, HI_UNF_AVPLAY_ATTR_ID_FRMRATE_PARAM, &fp);
        if (getenv("VPLAY_DBG")) fprintf(stderr, "fps %u.%03u %x\n", fp.stSetFrmRate.u32fpsInteger, fp.stSetFrmRate.u32fpsDecimal, r);
    }
    if (getenv("VPLAY_SYNCFRC")) {
        HI_U32 mode = 1;
        HI_S32 r = HI_UNF_AVPLAY_Invoke(hAv, HI_UNF_AVPLAY_INVOKE_SET_SYNC_MODE, &mode);
        if (getenv("VPLAY_DBG")) fprintf(stderr, "syncfrc %x\n", r);
    }
    if (getenv("VPLAY_QUICK")) {
        HI_S32 r = HI_UNF_VO_SetQuickOutputEnable(hWin, HI_TRUE);
        if (getenv("VPLAY_DBG")) fprintf(stderr, "quick %x\n", r);
    }
    if (HI_UNF_AVPLAY_Start(hAv, HI_UNF_AVPLAY_MEDIA_CHAN_VID, HI_NULL)) return -8;
    return 0;
}

static void video_close(void)
{
    if (hAv) {
        HI_UNF_AVPLAY_STOP_OPT_S st = { HI_UNF_AVPLAY_STOP_MODE_BLACK, 0 };
        HI_UNF_AVPLAY_Stop(hAv, HI_UNF_AVPLAY_MEDIA_CHAN_VID, &st);
        if (hWin) { HI_UNF_VO_SetWindowEnable(hWin, HI_FALSE); HI_UNF_VO_DetachWindow(hWin, hAv); }
        HI_UNF_AVPLAY_ChnClose(hAv, HI_UNF_AVPLAY_MEDIA_CHAN_VID);
        HI_UNF_AVPLAY_Destroy(hAv);
    }
    if (hWin) HI_UNF_VO_DestroyWindow(hWin);
    if (hi_up) { HI_UNF_AVPLAY_DeInit(); HI_UNF_VO_DeInit(); HI_UNF_DISP_DeInit(); HI_SYS_DeInit(); }
}

static int64_t vbase = -1, fdur = 40;   /* pts of the first frame since start/seek; frame length ms */
static HI_U32 vcnt0, vdrop0;             /* avplay counters at that moment */
static int vneedbase = 1;

static void vcount_mark(void)
{
    HI_UNF_AVPLAY_STATUS_INFO_S si; memset(&si, 0, sizeof si);
    HI_UNF_AVPLAY_GetStatusInfo(hAv, &si);
    vcnt0 = si.u32VidFrameCount; vdrop0 = si.u32VidFrameDropCount;
}

static int win_queued(void)              /* frames waiting in our VO window (-1 = unknown) */
{
    static char path[32];
    char l[256]; int q = -1;
    if (!path[0]) {                      /* find our window: the one showing the full-screen output rect */
        for (int i = 0; i < 4 && !path[0]; i++) for (int j = 0; j < 2 && !path[0]; j++) {
            char pth[32]; snprintf(pth, sizeof pth, "/proc/msp/win%02d%02d", j, i);
            FILE *f = fopen(pth, "r"); if (!f) continue;
            int ok = 0;
            while (fgets(l, sizeof l, f)) if (strstr(l, "Out(X/Y/W/H)") && strstr(l, "1920/1080")) ok = 1;
            fclose(f);
            if (ok) strcpy(path, pth);
        }
        if (!path[0]) return -1;
    }
    FILE *f = fopen(path, "r"); if (!f) return -1;
    while (fgets(l, sizeof l, f)) { char *t = strstr(l, "ToDisp["); if (t) q = atoi(t + 7); }
    fclose(f);
    return q;
}

static volatile int64_t ev_pts = -1, ev_src = -1, ev_t;   /* last frame handed to the window */
static volatile int ev_n;
static HI_S32 on_event(HI_HANDLE h, HI_UNF_AVPLAY_EVENT_E ev, HI_VOID *para)
{
    if (ev == HI_UNF_AVPLAY_EVENT_NEW_VID_FRAME && para) {
        HI_UNF_VIDEO_FRAME_INFO_S *f = (HI_UNF_VIDEO_FRAME_INFO_S *)para;
        ev_pts = f->u32Pts; ev_src = f->u32SrcPts; ev_t = now_ms(); ev_n++;
    }
    return HI_SUCCESS;
}

static int video_put(VPkt *p)
{
    HI_UNF_STREAM_BUF_S buf;
    if (HI_UNF_AVPLAY_GetBuf(hAv, HI_UNF_AVPLAY_BUF_ID_ES_VID, p->len, &buf, 0) != HI_SUCCESS) return -1;
    memcpy(buf.pu8Data, p->d, p->len);
    HI_UNF_AVPLAY_PutBuf(hAv, HI_UNF_AVPLAY_BUF_ID_ES_VID, p->len, (p->pts >= 0 && !getenv("VPLAY_NOPTS")) ? (HI_U32)p->pts : 0xffffffff);
    if (vneedbase) { vneedbase = 0; vbase = p->pts >= 0 ? p->pts : p->dts; }
    return 0;
}


static void capture(const char *path)          /* debug: save what the window shows (Y plane) */
{
    HI_UNF_VIDEO_FRAME_INFO_S cap; memset(&cap, 0, sizeof cap);
    HI_S32 r = HI_UNF_VO_CapturePicture(hWin, &cap);
    printf("CAP %x %ux%u\n", r, cap.u32Width, cap.u32Height);
    if (r || !cap.stVideoFrameAddr[0].u32YAddr) return;
    HI_U8 *y = (HI_U8 *)HI_MPI_MMZ_Map(cap.stVideoFrameAddr[0].u32YAddr, HI_FALSE);
    if (y) { FILE *o = fopen(path, "wb"); fprintf(o, "P5 %u %u 255\n", cap.u32Width, cap.u32Height);
        for (HI_U32 row = 0; row < cap.u32Height; row++) fwrite(y + row * cap.stVideoFrameAddr[0].u32YStride, 1, cap.u32Width, o); fclose(o); }
    HI_UNF_VO_CapturePictureRelease(hWin, &cap);
}

/* ---------------- queues ---------------- */
static void queues_clear(void)
{
    pthread_mutex_lock(&m);
    while (aq) { AChunk *n = aq->next; free(aq); aq = n; } aqt = NULL;
    while (vq) { VPkt *n = vq->next; free(vq); vq = n; } vqt = NULL; vqn = 0;
    aclock = -1; seekgen++;
    pthread_cond_broadcast(&c);
    pthread_mutex_unlock(&m);
}

int main(int argc, char **argv)
{
    AVFormatContext *fmt = NULL;
    AVCodecContext *adec = NULL;
    AVBSFContext *bsf = NULL;
    SwrContext *swr = NULL;
    AVPacket *pkt = av_packet_alloc(), *vp = av_packet_alloc();
    AVFrame *fr = av_frame_alloc();
    int vs = -1, as = -1, eof = 0, rc = 0;
    int64_t lead = getenv("VPLAY_LEAD") ? atoll(getenv("VPLAY_LEAD")) : (getenv("VPLAY_SYNC") && !atoi(getenv("VPLAY_SYNC")) ? 120 : 180);
    int64_t last_status = 0, dur = 0, last_sync = 0;
    int avofs = getenv("VPLAY_AVOFS") ? atoi(getenv("VPLAY_AVOFS")) : 0;   /* + = show picture later */
    int64_t vhold_until = 0, esum = 0; int nsync = 0;
    pthread_t ath;
    char line[128]; int ll = 0;

    if (argc < 2) { fprintf(stderr, "usage: vplay file [start] [volume]\n"); return 1; }
    signal(SIGINT, onsig); signal(SIGTERM, onsig); signal(SIGPIPE, SIG_IGN);
    setvbuf(stdout, NULL, _IOLBF, 0);
    set_volume(argc > 3 ? atoi(argv[3]) : 99);
    fcntl(0, F_SETFL, fcntl(0, F_GETFL) | O_NONBLOCK);

    if (avformat_open_input(&fmt, argv[1], NULL, NULL) < 0 || avformat_find_stream_info(fmt, NULL) < 0) {
        printf("ERR cannot open file\n"); return 2;
    }
    vs = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    as = av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (vs < 0) { printf("ERR no video\n"); return 2; }
    AVCodecParameters *vpar = fmt->streams[vs]->codecpar;
    int hevc = vpar->codec_id == AV_CODEC_ID_HEVC;
    if (!hevc && vpar->codec_id != AV_CODEC_ID_H264) { printf("ERR video format not supported (needs H.264 or H.265)\n"); return 3; }
    dur = fmt->duration > 0 ? fmt->duration / 1000 : 0;
    {   AVRational fr = fmt->streams[vs]->avg_frame_rate.num ? fmt->streams[vs]->avg_frame_rate : fmt->streams[vs]->r_frame_rate;
        if (fr.num > 0 && fr.den > 0) fdur = (int64_t)fr.den * 1000 / fr.num; }

    const AVBitStreamFilter *bf = av_bsf_get_by_name(hevc ? "hevc_mp4toannexb" : "h264_mp4toannexb");
    av_bsf_alloc(bf, &bsf);
    avcodec_parameters_copy(bsf->par_in, vpar);
    bsf->time_base_in = fmt->streams[vs]->time_base;
    av_bsf_init(bsf);

    if (getenv("VPLAY_NOAUDIO")) as = -1;
    if (as >= 0) {
        AVCodecParameters *apar = fmt->streams[as]->codecpar;
        const AVCodec *ac = avcodec_find_decoder(apar->codec_id);
        if (ac && (adec = avcodec_alloc_context3(ac)) && avcodec_parameters_to_context(adec, apar) >= 0 &&
            avcodec_open2(adec, ac, NULL) >= 0 && audio_open() == 0) {
            int64_t lay = adec->channel_layout ? adec->channel_layout : av_get_default_channel_layout(adec->channels);
            swr = swr_alloc_set_opts(NULL, AV_CH_LAYOUT_STEREO, AV_SAMPLE_FMT_S16, RATE, lay, adec->sample_fmt, adec->sample_rate, 0, NULL);
            if (swr && swr_init(swr) >= 0) has_audio = 1;
        }
        if (!has_audio) { printf("ERR sound format not supported, playing without sound\n"); as = -1; }
    }
    if ((rc = video_open(hevc, vpar->width, vpar->height, fdur))) { printf("ERR decoder start failed (%d)\n", rc); video_close(); return 4; }
    vcount_mark();
    if (has_audio) pthread_create(&ath, NULL, audio_thread, NULL);
    sys_base = now_ms();

    if (argc > 2 && atof(argv[2]) > 0) {
        av_seek_frame(fmt, -1, (int64_t)(atof(argv[2]) * AV_TIME_BASE), AVSEEK_FLAG_BACKWARD);
        if (!has_audio) sys_base -= (int64_t)(atof(argv[2]) * 1000);
    }

    while (!quit) {
        /* ---- commands ---- */
        char ch;
        while (read(0, &ch, 1) == 1) {
            if (ch != '\n') { if (ll < (int)sizeof line - 1) line[ll++] = ch; continue; }
            line[ll] = 0; ll = 0;
            if (line[0] == 'q') quit = 1;
            else if (line[0] == 'c') capture(line[1] ? line + 2 : "/tmp/vcap.pgm");
            else if (line[0] == 'v') set_volume(atoi(line + 1));
            else if (line[0] == 'p') {
                pthread_mutex_lock(&m);
                if (!has_audio) sys_base = paused ? now_ms() - sys_base : now_ms() - sys_base;
                paused = !paused;
                pthread_cond_broadcast(&c);
                pthread_mutex_unlock(&m);
                vhold_until = 0; ahold_until = 0; asilence = 0; last_sync = now_ms() + 1000; nsync = 0; esum = 0;
                if (paused) HI_UNF_AVPLAY_Pause(hAv, HI_NULL); else HI_UNF_AVPLAY_Resume(hAv, HI_NULL);
            } else if (line[0] == 's') {
                int64_t t = (int64_t)(atof(line + 1) * 1000); if (t < 0) t = 0;
                queues_clear();
                av_seek_frame(fmt, -1, t * 1000, AVSEEK_FLAG_BACKWARD);
                if (adec) avcodec_flush_buffers(adec);
                av_bsf_flush(bsf);
                HI_UNF_AVPLAY_Reset(hAv, HI_NULL);
                vcount_mark(); vneedbase = 1; vbase = -1; ev_pts = -1; ahold_until = 0; asilence = 0;
                if (vhold_until) { vhold_until = 0; if (!paused) HI_UNF_AVPLAY_Resume(hAv, HI_NULL); }
                last_sync = now_ms() + 1000; nsync = 0; esum = 0;
                if (!has_audio) sys_base = paused ? t : now_ms() - t;
                eof = 0;
            }
        }
        if (quit) break;

        int64_t clk = clock_now();
        /* ---- read ahead ---- */
        for (int k = 0; k < 16 && !eof; k++) {
            pthread_mutex_lock(&m);
            int64_t ahead = aqt ? aqt->pts + (int64_t)aqt->n * 1000 / RATE - clk : 0;
            int nv = vqn;
            pthread_mutex_unlock(&m);
            if (!(has_audio ? (ahead < 200 && nv < 2000) || (ahead < 2500 && nv < 150) : nv < 50)) break;
            int r = av_read_frame(fmt, pkt);
            if (r < 0) { eof = 1; av_bsf_send_packet(bsf, NULL); }
            else if (pkt->stream_index == vs) { av_bsf_send_packet(bsf, pkt); }
            else if (pkt->stream_index == as) {
                AVRational tb = fmt->streams[as]->time_base;
                if (avcodec_send_packet(adec, pkt) >= 0)
                    while (avcodec_receive_frame(adec, fr) >= 0) {
                        int64_t pts = fr->best_effort_timestamp != AV_NOPTS_VALUE ? av_rescale_q(fr->best_effort_timestamp, tb, (AVRational){1, 1000}) : 0;
                        int max = swr_get_out_samples(swr, fr->nb_samples) + 32;
                        AChunk *a = malloc(sizeof(AChunk) + max * 4);
                        uint8_t *out = (uint8_t *)a->s;
                        a->n = swr_convert(swr, &out, max, (const uint8_t **)fr->extended_data, fr->nb_samples);
                        a->pts = pts; a->off = 0; a->raw = 0; a->next = NULL;
                        if (a->n <= 0) { free(a); continue; }
                        pthread_mutex_lock(&m);
                        if (aqt) aqt->next = a; else aq = a; aqt = a;
                        pthread_cond_broadcast(&c);
                        pthread_mutex_unlock(&m);
                    }
            }
            if (r >= 0) av_packet_unref(pkt);
            while (av_bsf_receive_packet(bsf, vp) >= 0) {
                AVRational tb = fmt->streams[vs]->time_base;
                VPkt *v = malloc(sizeof(VPkt) + vp->size);
                v->dts = vp->dts != AV_NOPTS_VALUE ? av_rescale_q(vp->dts, tb, (AVRational){1, 1000}) : (vqt ? vqt->dts + 40 : 0);
                v->pts = vp->pts != AV_NOPTS_VALUE ? av_rescale_q(vp->pts, tb, (AVRational){1, 1000}) : -1;
                v->len = vp->size; memcpy(v->d, vp->data, vp->size); v->next = NULL;
                pthread_mutex_lock(&m);
                if (vqt) vqt->next = v; else vq = v; vqt = v; vqn++;
                pthread_mutex_unlock(&m);
                av_packet_unref(vp);
            }
        }

        /* ---- feed the decoder what is due ---- */
        clk = clock_now();
        int64_t pclk = clk;                     /* feed by the picture's own position, so holding the */
        if (ev_pts >= 0 && getenv("VPLAY_PCLK")) {   /* sound (or the picture) never starves the decoder */
            int64_t vs_ = ev_pts - 200 + (now_ms() - ev_t);
            if (vs_ > pclk) pclk = vs_;
        }
        while (!paused) {
            pthread_mutex_lock(&m);
            VPkt *v = vq;
            if (!v || v->dts > pclk + lead) { pthread_mutex_unlock(&m); break; }
            pthread_mutex_unlock(&m);
            if (video_put(v) < 0) break;            /* decoder buffer full: try again shortly */
            pthread_mutex_lock(&m);
            vq = v->next; if (!vq) vqt = NULL; vqn--;
            pthread_mutex_unlock(&m);
            free(v);
        }

        /* ---- A/V sync: this firmware never gets PTS into its sync module, so the window
         * free-runs at the stream frame rate.  We know which frame is on screen from the count
         * of frames handed to the window minus those still queued there, and nudge whichever
         * side is early: hold the picture (avplay pause) or hold the sound (stop writing). ---- */
        if (vhold_until && now_ms() >= vhold_until) {
            vhold_until = 0;
            if (!paused) HI_UNF_AVPLAY_Resume(hAv, HI_NULL);
            last_sync = now_ms() + 500; nsync = 0; esum = 0;
        }
        /* picture on screen now = last frame handed to the window, minus the window queue */
        if (!getenv("VPLAY_NOCORR") && has_audio && !paused && !vhold_until && aclock >= 0 && ev_pts >= 0 &&
                ahold_until < now_ms() && now_ms() >= last_sync + 250) {
            HI_U32 dly = 0;
            int64_t age = now_ms() - ev_t;
            if (age < 300 && HI_UNF_VO_GetDelayMs(hWin, &dly) == HI_SUCCESS) {
                int64_t e = ev_pts - dly + age - (aclock + avofs);     /* + = picture early */
                last_sync = now_ms();
                if (e > -3000 && e < 3000) { esum += e; nsync++; }
                if (nsync == 4) {                                     /* act on the average of 4 readings (1 s) */
                    int64_t mm = esum / 4;
                    if (getenv("VPLAY_DBG")) fprintf(stderr, "S a=%lld e=%lld lead=%lld\n", (long long)aclock, (long long)mm, (long long)lead);
                    /* + = picture early: feed it less far ahead (and the other way round) */
                    if (mm > 30 || mm < -30) {
                        lead -= mm * 3 / 4;
                        if (lead < 80) lead = 80;
                        if (lead > 1500) lead = 1500;
                    }
                    nsync = 0; esum = 0;
                }
            }
        }
        if (now_ms() - last_status >= 500) {
            last_status = now_ms();
            if (getenv("VPLAY_DBG")) {
                HI_U32 dly = 0; HI_UNF_VO_GetDelayMs(hWin, &dly);
                fprintf(stderr, "E t=%lld a=%lld evpts=%lld evsrc=%lld age=%lld evn=%d vodelay=%u\n", (long long)now_ms() % 100000,
                    (long long)clk, (long long)ev_pts, (long long)ev_src, (long long)(now_ms() - ev_t), ev_n, dly);
                HI_UNF_AVPLAY_STATUS_INFO_S si; memset(&si, 0, sizeof si);
                if (atoi(getenv("VPLAY_DBG")) == 2) HI_UNF_AVPLAY_GetStatusInfo(hAv, &si);
                if (atoi(getenv("VPLAY_DBG")) == 2) fprintf(stderr, "D clk=%lld lastvid=%u play=%u local=%u frames=%u drop=%u fbuf=%u fbt=%u esused=%u vq=%d head=%lld\n",
                    (long long)clk, si.stSyncStatus.u32LastVidPts, si.stSyncStatus.u32PlayTime, si.stSyncStatus.u32LocalTime,
                    si.u32VidFrameCount, si.u32VidFrameDropCount, si.stBufStatus[HI_UNF_AVPLAY_BUF_ID_ES_VID].u32FrameBufNum,
                    si.stBufStatus[HI_UNF_AVPLAY_BUF_ID_ES_VID].u32FrameBufTime, si.stBufStatus[HI_UNF_AVPLAY_BUF_ID_ES_VID].u32UsedSize,
                    vqn, vq ? (long long)vq->dts : -1LL);
            }
            int64_t start = fmt->start_time != AV_NOPTS_VALUE ? fmt->start_time / 1000 : 0;
            printf("T %lld %lld %d\n", (long long)(clk - start), (long long)dur, paused);
        }
        pthread_mutex_lock(&m);
        int done = eof && !vq && !aq;
        pthread_mutex_unlock(&m);
        if (done) { usleep(has_audio ? 400000 : 200000); printf("E\n"); break; }
        usleep(5000);
    }

    quit = 1; pthread_cond_broadcast(&c);
    if (has_audio) { pthread_join(ath, NULL); snd_pcm_drop(pcm); snd_pcm_close(pcm); }
    video_close();
    queues_clear();
    avformat_close_input(&fmt);
    return 0;
}

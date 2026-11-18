/* Experimental H.264 byte-stream sink for CM101s-2. Not production software. */
#include <gst/gst.h>
#include <string.h>
#include "hi_common.h"
#include "hi_unf_avplay.h"
#include "hi_unf_vo.h"
#include "hi_unf_disp.h"

typedef struct { GstElement parent; GstPad *sink; HI_HANDLE av, win; int init, channel, attached, started; unsigned int probes, acquired; HI_S32 probe_error; } GstHisiSink;
typedef struct { GstElementClass parent; } GstHisiSinkClass;
G_DEFINE_TYPE(GstHisiSink, gst_hisi_sink, GST_TYPE_ELEMENT)

static GstStaticPadTemplate sink_template = GST_STATIC_PAD_TEMPLATE("sink", GST_PAD_SINK, GST_PAD_ALWAYS,
    GST_STATIC_CAPS("video/x-h264, stream-format=(string)byte-stream"));

static volatile gint frame_count;
static HI_S32 on_frame(HI_HANDLE h, HI_UNF_AVPLAY_EVENT_E e, HI_VOID *data) {
    (void)h;
    /* Callback API has no userdata; only one test instance is supported. */
    if (e == HI_UNF_AVPLAY_EVENT_NEW_VID_FRAME) {
        if (g_getenv("HISI_PROBE_FRAME") && data && g_atomic_int_get(&frame_count) == 0) {
            HI_UNF_VIDEO_FRAME_INFO_S *f = (HI_UNF_VIDEO_FRAME_INFO_S *)data;
            g_printerr("hisi264testsink: callback frame %ux%u format=%d Y_present=%d C_present=%d stride=%u\n",
                f->u32Width, f->u32Height, f->enVideoFormat, f->stVideoFrameAddr[0].u32YAddr != 0,
                f->stVideoFrameAddr[0].u32CAddr != 0, f->stVideoFrameAddr[0].u32YStride);
        }
        g_atomic_int_inc(&frame_count);
    }
    return HI_SUCCESS;
}

static void close_hw(GstHisiSink *s) {
    g_printerr("hisi264testsink: frame callbacks=%d\n", g_atomic_int_get(&frame_count));
    if (g_getenv("HISI_PROBE_FRAME")) g_printerr("hisi264testsink: acquire attempts=%u acquired=%u last_error=0x%08x\n", s->probes, s->acquired, (unsigned)s->probe_error);
    if (s->started) { HI_UNF_AVPLAY_STOP_OPT_S o = { HI_UNF_AVPLAY_STOP_MODE_BLACK, 0 }; HI_UNF_AVPLAY_Stop(s->av, HI_UNF_AVPLAY_MEDIA_CHAN_VID, &o); }
    if (s->attached) { HI_UNF_VO_SetWindowEnable(s->win, HI_FALSE); HI_UNF_VO_DetachWindow(s->win, s->av); }
    if (s->channel) HI_UNF_AVPLAY_ChnClose(s->av, HI_UNF_AVPLAY_MEDIA_CHAN_VID);
    if (s->av) HI_UNF_AVPLAY_Destroy(s->av);
    if (s->win) HI_UNF_VO_DestroyWindow(s->win);
    if (s->init) { HI_UNF_AVPLAY_DeInit(); HI_UNF_VO_DeInit(); HI_UNF_DISP_DeInit(); HI_SYS_DeInit(); }
    s->av = s->win = 0; s->started = s->attached = s->channel = s->init = 0;
}

static int open_hw(GstHisiSink *s) {
    HI_UNF_AVPLAY_ATTR_S attr; HI_UNF_SYNC_ATTR_S sync; HI_UNF_VCODEC_ATTR_S dec;
    HI_UNF_WINDOW_ATTR_S win; HI_UNF_AVPLAY_OPEN_OPT_S opt; HI_UNF_AVPLAY_FRMRATE_PARAM_S fps;
    g_atomic_int_set(&frame_count, 0);
    if (HI_SYS_Init() || HI_UNF_DISP_Init() || HI_UNF_VO_Init(HI_UNF_VO_DEV_MODE_NORMAL) || HI_UNF_AVPLAY_Init()) return -1;
    s->init = 1;
    memset(&win, 0, sizeof win); win.enDisp = HI_UNF_DISPLAY1;
    win.bVirtual = g_getenv("HISI_VIRTUAL_WINDOW") ? HI_TRUE : HI_FALSE;
    win.stWinAspectAttr.enAspectCvrs = HI_UNF_VO_ASPECT_CVRS_LETTERBOX;
    win.stOutputRect.s32Width = 1920; win.stOutputRect.s32Height = 1080;
    if (HI_UNF_VO_CreateWindow(&win, &s->win)) goto fail;
    if (HI_UNF_AVPLAY_GetDefaultConfig(&attr, HI_UNF_AVPLAY_STREAM_TYPE_ES)) goto fail;
    attr.stStreamAttr.u32VidBufSize = 4 * 1024 * 1024;
    if (HI_UNF_AVPLAY_Create(&attr, &s->av)) goto fail;
    if (HI_UNF_AVPLAY_GetAttr(s->av, HI_UNF_AVPLAY_ATTR_ID_SYNC, &sync)) goto fail;
    sync.enSyncRef = HI_UNF_SYNC_REF_NONE;
    if (HI_UNF_AVPLAY_SetAttr(s->av, HI_UNF_AVPLAY_ATTR_ID_SYNC, &sync)) goto fail;
    memset(&opt, 0, sizeof opt); opt.enDecType = HI_UNF_VCODEC_DEC_TYPE_NORMAL;
    opt.enCapLevel = HI_UNF_VCODEC_CAP_LEVEL_720P; opt.enProtocolLevel = HI_UNF_VCODEC_PRTCL_LEVEL_H264;
    if (HI_UNF_AVPLAY_ChnOpen(s->av, HI_UNF_AVPLAY_MEDIA_CHAN_VID, &opt)) goto fail;
    s->channel = 1;
    if (HI_UNF_AVPLAY_GetAttr(s->av, HI_UNF_AVPLAY_ATTR_ID_VDEC, &dec)) goto fail;
    dec.enType = HI_UNF_VCODEC_TYPE_H264; dec.enMode = HI_UNF_VCODEC_MODE_NORMAL; dec.u32ErrCover = 100; dec.u32Priority = 3;
    if (HI_UNF_AVPLAY_SetAttr(s->av, HI_UNF_AVPLAY_ATTR_ID_VDEC, &dec)) goto fail;
    if (HI_UNF_VO_AttachWindow(s->win, s->av)) goto fail;
    s->attached = 1;
    if (HI_UNF_VO_SetWindowEnable(s->win, HI_TRUE)) goto fail;
    HI_UNF_AVPLAY_RegisterEvent64(s->av, HI_UNF_AVPLAY_EVENT_NEW_VID_FRAME, on_frame);
    memset(&fps, 0, sizeof fps); fps.enFrmRateType = HI_UNF_AVPLAY_FRMRATE_TYPE_USER; fps.stSetFrmRate.u32fpsInteger = 60;
    HI_UNF_AVPLAY_SetAttr(s->av, HI_UNF_AVPLAY_ATTR_ID_FRMRATE_PARAM, &fps);
    if (HI_UNF_AVPLAY_Start(s->av, HI_UNF_AVPLAY_MEDIA_CHAN_VID, HI_NULL)) goto fail;
    s->started = 1; return 0;
fail:
    close_hw(s); return -1;
}

static GstFlowReturn chain(GstPad *pad, GstObject *parent, GstBuffer *input) {
    GstHisiSink *s = (GstHisiSink *)parent; GstMapInfo map; HI_UNF_STREAM_BUF_S dst;
    int tries = 0; GstFlowReturn ret = GST_FLOW_OK; (void)pad;
    if (!gst_buffer_map(input, &map, GST_MAP_READ)) { gst_buffer_unref(input); return GST_FLOW_ERROR; }
    while (HI_UNF_AVPLAY_GetBuf(s->av, HI_UNF_AVPLAY_BUF_ID_ES_VID, map.size, &dst, 0) != HI_SUCCESS) {
        if (++tries > 500) { ret = GST_FLOW_ERROR; break; } g_usleep(2000);
    }
    if (ret == GST_FLOW_OK) {
        memcpy(dst.pu8Data, map.data, map.size);
        if (HI_UNF_AVPLAY_PutBuf(s->av, HI_UNF_AVPLAY_BUF_ID_ES_VID, map.size, 0xffffffff) != HI_SUCCESS) ret = GST_FLOW_ERROR;
    }
    if (g_getenv("HISI_PROBE_FRAME") && g_atomic_int_get(&frame_count) > 0 && s->probes < 20 && !s->acquired) {
        HI_UNF_VIDEO_FRAME_INFO_S frame; HI_S32 result;
        memset(&frame, 0, sizeof frame); ++s->probes;
        result = HI_UNF_VO_AcquireFrame(s->win, &frame, 0); s->probe_error = result;
        if (result == HI_SUCCESS) {
            if (frame.u32Width && frame.u32Height && frame.stVideoFrameAddr[0].u32YAddr) ++s->acquired;
            g_printerr("hisi264testsink: acquired frame %ux%u format=%d Y_present=%d C_present=%d stride=%u\n",
                frame.u32Width, frame.u32Height, frame.enVideoFormat,
                frame.stVideoFrameAddr[0].u32YAddr != 0, frame.stVideoFrameAddr[0].u32CAddr != 0,
                frame.stVideoFrameAddr[0].u32YStride);
            HI_UNF_VO_ReleaseFrame(s->win, &frame);
        }
    }
    gst_buffer_unmap(input, &map); gst_buffer_unref(input); return ret;
}

static gboolean sink_event(GstPad *pad, GstObject *parent, GstEvent *event) {
    if (GST_EVENT_TYPE(event) == GST_EVENT_EOS) {
        g_usleep(1000000);
        gst_event_unref(event);
        return gst_element_post_message(GST_ELEMENT(parent), gst_message_new_eos(parent));
    }
    return gst_pad_event_default(pad, parent, event);
}

static GstStateChangeReturn change_state(GstElement *element, GstStateChange transition) {
    GstHisiSink *s = (GstHisiSink *)element; GstStateChangeReturn ret;
    if (transition == GST_STATE_CHANGE_READY_TO_PAUSED && open_hw(s)) {
        GST_ELEMENT_ERROR(s, RESOURCE, OPEN_WRITE, ("Cannot open HiSilicon decoder"), (NULL)); return GST_STATE_CHANGE_FAILURE;
    }
    ret = GST_ELEMENT_CLASS(gst_hisi_sink_parent_class)->change_state(element, transition);
    if (ret == GST_STATE_CHANGE_FAILURE && transition == GST_STATE_CHANGE_READY_TO_PAUSED) close_hw(s);
    if (transition == GST_STATE_CHANGE_PAUSED_TO_READY) close_hw(s);
    return ret;
}

static void gst_hisi_sink_class_init(GstHisiSinkClass *klass) {
    GstElementClass *ec = GST_ELEMENT_CLASS(klass);
    gst_element_class_set_static_metadata(ec, "Experimental HiSilicon H.264 sink", "Sink/Video", "Test direct hardware output", "Prototype");
    gst_element_class_add_static_pad_template(ec, &sink_template); ec->change_state = change_state;
}
static void gst_hisi_sink_init(GstHisiSink *s) {
    s->sink = gst_pad_new_from_static_template(&sink_template, "sink");
    gst_pad_set_chain_function(s->sink, GST_DEBUG_FUNCPTR(chain)); gst_element_add_pad(GST_ELEMENT(s), s->sink);
    gst_pad_set_event_function(s->sink, GST_DEBUG_FUNCPTR(sink_event));
    GST_OBJECT_FLAG_SET(s, GST_ELEMENT_FLAG_SINK);
}
static gboolean plugin_init(GstPlugin *plugin) { return gst_element_register(plugin, "hisi264testsink", GST_RANK_NONE, gst_hisi_sink_get_type()); }
#ifndef PACKAGE
#define PACKAGE "hisi-test"
#endif
GST_PLUGIN_DEFINE(GST_VERSION_MAJOR, GST_VERSION_MINOR, hisitest, "Experimental HiSilicon sink", plugin_init, "0.1", "LGPL", "hisi-test", "https://example.invalid")

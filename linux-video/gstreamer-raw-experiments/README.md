# Experimental GStreamer & Raw NV21 Decode Notes

These files are reference prototypes exploring two paths beyond `vplay.c`:

1. **`gsthisisink.c`**: A GStreamer 1.16 sink (`hisi264testsink`) accepting `video/x-h264,stream-format=byte-stream,alignment=au` and rendering through `HI_UNF_AVPLAY` + `HI_UNF_VO` (used by the patched `uxplay` AirPlay receiver).
2. **`direct_probe.c` / `hisi_raw.c` / `hisi_raw.h` / `hisi_raw_test.c` / `build_raw.sh`**: Bypasses `AVPLAY` and `VO` entirely by opening `VDEC -> VPSS` virtual port output to obtain **raw decoded `1280x720 NV21` frames** in MMZ memory.
   - **Important kernel ABI note:** The `SPC060` frame struct misinterprets the width field on this kernel; at that offset the running firmware actually places `(handle_or_tag, 1280, 720)` (3 words: `2490368, 1280, 720`). `direct_probe.c` validates MMZ allocation bounds before mapping Y/UV planes and releases every frame back to `VPSS`.
   - These remain experimental prototypes (fixed `1280x720 NV21`, no seeking or general V4L2/DRM wrapper). For reliable playback, see `linux-video/vplay/vplay.c`.

# Solving Linux Video & HDMI Output on the CM101s-2 (Hi3798MV100)

The China Mobile **CM101s-2** runs a HiSilicon **Hi3798MV100** SoC (quad-core Cortex-A7, 1 GB RAM). After flashing **HiNAS / Ubuntu 20.04** (`Linux 4.4.35_ecoo`), the kernel has HiSilicon's MSP media drivers compiled in (`/dev/hi_vdec`, `/dev/hi_vo`, `/dev/hi_disp`, `/dev/hi_vpss`, `/dev/hi_sync`, `/dev/hi_pm`, `/dev/hi_ir`), **but ships zero userspace media libraries, no V4L2 M2M `/dev/video*` decoder, and no `/dev/hi_omxvdec`**.

This document records how both **HDMI graphics output** and **hardware H.264/H.265 video decoding** were made to work reliably under standard Linux.

---

## 1. Graphics Layer: `/dev/fb0` (`hifb`)

The UI (`quran-tv/hdmi-status.py`) renders directly to `/dev/fb0` using Pillow (`PIL`) — **no X11, Wayland, or browser needed**:

- **Pixel format:** `1920x1080`, 32-bit `ARGB8888`, stride `7680` bytes/row (PIL writes RGBA, swapped to `BGRA` for little-endian `ARGB`).
- **Display setup:** `disp1` (`HDMI0`, `1080P60`) carries the graphics layer (`hifb0`) on top of the hardware video plane (`Zorder: VIDEO -> GFX`).
- **Critical `hifb` quirks discovered:**
  1. **Keeping `/dev/fb0` open:** The `hifb` layer is **only visible while a process holds `/dev/fb0` open**. Closing the file descriptor immediately blanks HDMI output, and layer settings reset on reopen.
  2. **Do not call `mmap.msync()`:** Calling `msync()` on `/dev/fb0`'s `mmap` returns `EINVAL`; write directly into the mapped buffer (`m[:total] = ...`).
  3. **Per-pixel alpha compositing:** Leaving `alpha = 0` (`(0, 0, 0, 0)`) on a rectangle makes the graphics layer transparent, revealing the hardware-decoded video layer underneath (used for full-screen playback overlays, OSD progress bars, and AirPlay prayer banners).
  4. **Boot splash handoff:** Early init redraws the boot logo over the screen, so `hdmi-status.service` waits until `/proc/uptime >= 60s` (`quran-tv/systemd/hdmi-status.boot-delay.conf`).

---

## 2. Why Standard Linux Decoders Don't Work Out of the Box

Checking the running box & original Android `system.img` (`p17_system.img`) confirmed:

- Android used `OMX.hisi.video.decoder` via `/dev/hi_omxvdec` (`libOMX.hisi.video.decoder.so` linked against Android `libc.so`, `libutils.so`, `libhi_msp.so`).
- The HiNAS Linux `4.4.35` kernel **omits `/dev/hi_omxvdec`** (no entry in `/proc/devices`, `/proc/misc`, or `/proc/kallsyms`) and leaves **`v4l2-mem2mem` disabled** (no `/dev/video*` decoder device).
- FFmpeg's `h264_v4l2m2m` / `h264_omx` have no kernel backend, and software decoding 720p/1080p H.265/H.264 on 4x Cortex-A7 cores saturates CPU and overheats the board.

However, the kernel **does** expose the full HiSilicon MSP driver stack (`/dev/hi_vdec`, `/dev/hi_vpss`, `/dev/hi_vo`, `/dev/hi_disp`, `/dev/hi_sync`, `/dev/hi_stat`, `/dev/mmz_userdev`), compiled around **UNF ~3.3.4**.

---

## 3. Rebuilding `libhimini.so` from HiSTB SDK + Kernel ABI Matching

Rather than guessing a new kernel build, we compiled a minimal userspace UNF shared library (`libhimini.so`) natively on the box using `gcc` from `JasonFreeLab/HiSTBLinuxV100R005C00SPC060` (closest open SDK, UNF 3.3.3) plus `stubs.c` for unused audio/demux/PQ symbols.

### Discovering Struct Size & Ioctl Differences

Because the running kernel (`2018102500` VDEC / `HiSTB_com_NAS_ubuntu_20220427`) was slightly newer than `SPC060`, several ioctls initially failed or mismatched struct sizes. Using `linux-video/probes/` (`probe2.py`, `probesize.py`, `probecmd.py`, `/proc/kallsyms`, and kernel error logs), we isolated the exact ABI deltas captured in `linux-video/hisdk-minimal/sdk-header-fixes.diff`:

| Component / Struct | SDK (`SPC060`) | Box Kernel (`4.4.35`) Fix | Notes |
|---|---|---|---|
| `DEF_HI_DRV_FRAME_INFO_SIZE` | `86` | `94` (+32 bytes per frame struct, 904 B total) | Found by probing ioctl size & `/dev/kmsg` |
| `HI_DRV_VPSS_PORT_CFG_S` | base size | +16 bytes reserved at end | Needed for VPSS port creation |
| `WIN_XDR_FRAME_S` | base size | +8 bytes at end | Required by VO `Window` attach/frame ioctls |
| `HI_VIDEO_PRE_CAP_*` & `VFMW GETCAP` | ioctl struct | Hard-coded std list (`HI_VIDEO_PRE_CAP_* = 0`) | Kernel `GETCAP` uses a 16-byte pointer struct |
| `C51_PMOC_VAL_S` (`/dev/hi_pm`) | smaller SDK struct | `252` bytes | Enables real IR suspend-to-RAM in `box-standby.py` |

Build files are in `linux-video/hisdk-minimal/`:
- `build.sh`: compiles selected SDK `common/api`, `msp/api/avplay`, `msp/api/vdec`, `msp/api/vo`, `msp/api/sync` objects with `-fPIC -mcpu=cortex-a7`.
- `stubs.c`: minimal stubs for `HI_MPI_ADEC_*`, `HI_MPI_AO_*`, `HI_MPI_DMX_*`, `HI_MPI_PQ_*`, `HI_MPI_PMOC_*`.
- `vtest.c`: standalone verification tool that feeds raw H.265/H.264 Annex B ES into `AVPLAY -> VO`.

---

## 4. Production Video Playback (`vplay.c`) & Solving 25 fps A/V Drift

Direct `HI_UNF_AVPLAY` ES playback (`linux-video/vplay/vplay.c`) links `libhimini.so` + FFmpeg (`libavformat`, `libavcodec`, `libswresample`) + ALSA (`plughw:0,0`):

- **Video:** Demuxed packets are sent raw (`H.264` / `H.265` Annex B) to `/dev/hi_vdec` via `HI_UNF_AVPLAY_GetBuf` / `PutBuf`.
- **Audio:** AAC/MP3/Opus audio is software-decoded via `libavcodec`, resampled to 48 kHz stereo S16LE, and played through ALSA (`plughw:0,0`) with software dB volume and a 0.5 s replay ring for glitch-free `snd_pcm_pause`/`drop` resume.

### Fixing the 25 fps Stutter / Audio Drift Bug

When declaring a file's native frame rate (e.g., `25 fps`) to `AVPLAY`, HiSilicon's `VO`/`VPSS` pipeline (driving HD `1080P60` on `display1` plus an attached NTSC `CVBS0` slave on `display0`) throttled the decoder down to ~14–18 fps after 6–15 seconds, causing video to lag steadily behind audio.

**Solution implemented in `vplay.c`:**
1. Set `HI_UNF_SYNC_REF_NONE` on `AVPLAY`.
2. Override `HI_UNF_AVPLAY_ATTR_ID_FRMRATE_PARAM` to **`USER 60 fps`** (matching the `1080P60` display refresh), so `VPSS`/`VO` never throttles decode rate (yielding steady 25.0 fps).
3. Pace frame injection from the C **audio clock** (`aclock + VPLAY_LEAD`, default `120–180 ms`), using `HI_UNF_AVPLAY_EVENT_NEW_VID_FRAME` minus `HI_UNF_VO_GetDelayMs()` in a feedback loop (`±30 ms` deadband) to nudge lead dynamically — achieving **±25–45 ms** A/V sync across pause and seek.

---

## 5. Memory Limits & Hardware Pitfalls (Avoid These!)

1. **`vmalloc` memory ceiling (`CAP_LEVEL_720P`):** The kernel keeps `VFMW_VDH_PRE` mapped (~154 MB), leaving only ~96 MB of `vmalloc` free. Opening 1080p VPSS channels frequently fails allocation; opening channels at `HI_UNF_VCODEC_CAP_LEVEL_720P` fits reliably.
2. **Never detach or close `display0` (`CVBS0`):** `display0` is attached as `destination` to `display1` (`HDMI0`). Closing/detaching `display0` leaves HDMI `display1` broken (`Get Out Info Failed`) until reboot.
3. **Avoid `PAL` + `1080p60` and `VO QuickOutput`:** Both freeze output on this kernel/EDID.
4. **Hardware buffer leak recovery:** Under `HI_UNF_VO`, rare kernel `drv_disp_bufcore.c` state assertion errors can leak `VPSS` port buffers until `HI_UNF_AVPLAY_GetBuf` fails permanently. Always detect sustained `GetBuf` starvation (>= 5 s without an accepted NAL) and tear down/recreate `AVPLAY` + `VO` windows cleanly (`HI_UNF_AVPLAY_Stop` -> `VO_DetachWindow` -> `AVPLAY_Destroy` -> `VO_DestroyWindow`).

---

## 6. AirPlay & Experimental GStreamer / Raw NV21 Paths

- **AirPlay Screen Mirroring (`linux-video/airplay/`):** `uxplay` patched (`uxplay.patch`) to route H.264 NAL units into a custom GStreamer sink (`hisi264testsink` in `linux-video/gstreamer-raw-experiments/gsthisisink.c`) rendering straight to `AVPLAY -> VO`, while calling `audio-control.py` to release the ALSA silence keeper before `alsasink` opens `plughw:0,0`.
- **Raw Decoded Frames (`direct_probe.c`, `hisi_raw.c`):** Demonstrates opening a standalone `VDEC -> VPSS` virtual port (`NV21`, `1280x720`) without `VO` or `AVPLAY`, validating the MMZ physical address, and `mmap`-ing decoded luma/chroma planes directly in Linux userspace. See `linux-video/gstreamer-raw-experiments/README.md` for details and ABI caveats.

# CM101s-2 Rust Rewrite

Native ARMv7 Rust workspace replacing the four runtime Python services (`quran-tv/*.py` + `audio-control.py`) with single static/minimal-dependency ELF binaries targeting **Ubuntu 20.04 (`armv7-unknown-linux-gnueabihf`, GLIBC 2.31)**:

| Crate | Replaces | Role |
|---|---|---|
| `quran-player` | `quran-tv/quran-player.py` | `127.0.0.1:8790` HTTP API (`tiny_http`), `mpg123 -R` daemon, ALSA `/dev/zero` silence keeper, `/dev/hi_ir` non-blocking remote key handler, video chooser + `vplay` controller, AirPlay handoff |
| `hdmi-status` | `quran-tv/hdmi-status.py` | Direct `/dev/fb0` (`1920x1080` BGRA ARGB8888 mmap) renderer with `ab_glyph` (`DejaVuSans.ttf`), `png` Mushaf mask & word/verse highlight blitting, offline MWL prayer times (`/etc/prayer-location.json`), remote "Prayed" mark, video/AirPlay/screen-off overlays, burn-in shift |
| `box-standby` | `quran-tv/box-standby.py` | Real suspend-to-RAM helper using `/dev/hi_pm` ioctls (`GET_WAKEUP`, `SET_WAKEUP`, `WAKEUP_READY`) and `/sys/power/state` |
| `audio-control` | `audio-control.py` | Calls `127.0.0.1:8790/airplay?act=claim_audio` when GStreamer/AirPlay claims ALSA |

> **Why `linux-video/vplay/vplay.c` stays C:** `vplay` is a direct FFI wrapper around HiSilicon's `libhimini.so` (`HI_UNF_AVPLAY_*`, `HI_UNF_VO_*`), FFmpeg (`libavformat`, `libavcodec`, `libswresample`), and ALSA (`libasound`), which links against the box's native libraries on the device.

## Cross-Compiling for CM101s-2 (GLIBC 2.31)

Using [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild):

```bash
rustup target add armv7-unknown-linux-gnueabihf
cargo install cargo-zigbuild
cd rust
cargo zigbuild --release --target armv7-unknown-linux-gnueabihf.2.31
```

Compiled stripped binaries are output to `rust/target/armv7-unknown-linux-gnueabihf/release/`:
- `quran-player`
- `hdmi-status`
- `box-standby`
- `audio-control`

## Installing on the Box (Drop-In Replacement)

Copy the four release binaries to `/usr/local/bin/` and point the systemd `ExecStart=` paths (or wrapper paths) at the compiled binaries instead of the Python scripts:

```bash
install -m 755 target/armv7-unknown-linux-gnueabihf/release/quran-player /usr/local/bin/quran-player
install -m 755 target/armv7-unknown-linux-gnueabihf/release/hdmi-status /usr/local/bin/hdmi-status
install -m 755 target/armv7-unknown-linux-gnueabihf/release/box-standby /usr/local/bin/box-standby
install -m 755 target/armv7-unknown-linux-gnueabihf/release/audio-control /usr/local/bin/audio-control
```

Keep `/home/ubuntu/quran/` (`quran.json`, `audio/`, `pages/`), `/usr/share/fonts/truetype/dejavu/DejaVuSans*.ttf`, `mpg123`, `aplay`, and `/etc/prayer-location.json` in place — the Rust binaries read the exact same state files, HTTP routes, and `/dev/*` interfaces.

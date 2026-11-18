# Installing Linux (HiNAS Ubuntu 20.04) on the CM101s-2

This guide documents how the stock Android TV firmware on the **China Mobile CM101s-2** (**Hi3798MV100**, 1 GB RAM, 8 GB eMMC) was replaced with **HiNAS / Ubuntu 20.04 (`Linux 4.4.35_ecoo`)**.

> **Warning:** Flashing the eMMC bootloader (`fastboot`) and partition table can brick the box if the wrong DDR training image or corrupted image is written. Back up the entire stock eMMC first and verify checksums before rebooting. Never run `otp_*` commands in fastboot.

---

## 1. Hardware & Serial UART Access

- **Board:** CM101s-2 (`Hi3798MV100`), 1 GB DDR3 (`16bitx2`, `4layers`), 8 GB eMMC (`7296 MiB`)
- **UART Header (`J18`):**
  - `pin 1` → `GND`
  - `pin 2` → adapter `TXD`
  - `pin 3` → adapter `RXD`
  - **Baud rate:** `115200 8N1`
- **USB-UART Driver Gotcha (Windows):** Many cheap CH340 USB-TTL adapters are clones; newer WCH drivers (`>= 3.8`, including `3.9` and `4.0`) fail to open the COM port (`device not functioning`). Install the older Microsoft-catalog CH340 driver `3.5.2019.1` if needed.

---

## 2. Inspect Stock Fastboot & Check Bootloader Locks

Interrupt boot right after `Bootrom start` by pressing **`Ctrl+C`** repeatedly on the UART console to enter `fastboot#`.

Check your board details before flashing:

```text
printenv
mmcinfo
```

Key things verified on our CM101s-2:
- `verify=n` (no signature verification blocking custom kernels)
- `Boot Env on eMMC` at `0x00100000` (`64 KiB`)
- Stock `DDRS` reg prints:
  `hi3798mdmo1a_hi3798mv100_ddr3_1gbyte_16bitx2_4layers_emmc.reg`

### Critical DDR Bootloader Note (`mdmo1g` vs `mdmo1a`)

Even though the stock Android fastboot prints `mdmo1a` in `Reg Name`, **the working HiNAS fastboot binary for this 1 GB CM101s-2 board is `mdmo1g`**:

```text
Reg Name: hi3798mdmo1g_hi3798mv100_ddr3_1gbyte_16bitx2_4layers_emmc.reg
Fastboot 3.3.0_ecoo_81082668-dirty
```

Verify your board's RAM size (`1GB`), bus width (`16bitx2`), and storage (`eMMC`) carefully when picking the matching HiNAS fastboot image.

---

## 3. Full Stock eMMC Backup First

Stock Android (`root@JM_HSMV100`) gives a root shell on the UART console once booted, and lacks full `dd`/`md5sum` applets.

1. Prepare a USB flash drive formatted with an ext4 or large-file-capable partition (or split chunks on FAT32) and plug it into the box.
2. Copy a static ARMv7 `busybox` binary (e.g. via ADB or USB stick) to `/data/local/tmp/b/busybox` and `chmod 755` it.
3. Back up:
   - All stock partitions (`mmcblk0p1` through `mmcblk0p21`) listed in `/proc/partitions`
   - Hardware boot areas `/dev/block/mmcblk0boot0` and `/dev/block/mmcblk0boot1`
   - Save MD5 checksums of every partition dump so you can restore Android or inspect original binaries (like `baseparam` / `pqparam` / `logo` / `system`) later.

---

## 4. Building & Flashing the Single `emmc.flash` Image

HiNAS images for Hi3798MV100 use the following eMMC GPT/blkdevparts layout:

```text
blkdevparts=mmcblk0:1M(boot),1M(bootargs),4M(baseparam),4M(pqparam),4M(logo),20M(kernel),64M(busybox),512M(backup),-(ubuntu)
```

With kernel parameters:

```text
model=mv100 console=ttyAMA0,115200 root=/dev/mmcblk0p9 rootfstype=ext4 rootwait mem=1G mmz=ddr,0,0,256M vmalloc=500M
```

Notice the partition boundaries inside the concatenated raw eMMC image (`emmc.flash`, `1,507,688,448` bytes / ~1.4 GiB):

| Offset | Size | Partition | Contents |
|---|---|---|---|
| `0 MiB` | `1 MiB` | `mmcblk0p1` (`boot`) | HiNAS Fastboot (`mdmo1g`) |
| `1 MiB` | `1 MiB` | `mmcblk0p2` (`bootargs`) | HiNAS U-Boot env (patched with your board's MAC `ethaddr=...`) |
| `2 MiB` | `4 MiB` | `mmcblk0p3` (`baseparam`) | Display/HDMI base parameters |
| `6 MiB` | `4 MiB` | `mmcblk0p4` (`pqparam`) | Picture quality params |
| `10 MiB` | `4 MiB` | `mmcblk0p5` (`logo`) | Boot splash partition (`LOGO_TABLE` + baseline JPEG at `0x2000`) |
| `14 MiB` | `20 MiB` | `mmcblk0p6` (`kernel`) | `Linux-4.4.35_ecoo_81082668` uncompressed Legacy uImage (`mmc read` at `28672` blocks) |
| `34 MiB` | `64 MiB` | `mmcblk0p7` (`busybox`) | Recovery / initramfs root |
| `98 MiB` | `512 MiB` | `mmcblk0p8` (`backup`) | HiNAS backup slot |
| `610 MiB` | remainder | `mmcblk0p9` (`ubuntu`) | Ubuntu 20.04.6 LTS ARMv7 rootfs (`ext4`) |

> **Tip:** Patch your original Ethernet MAC address (`ethaddr=xx:xx:xx:xx:xx:xx` from stock `printenv`) into the `bootargs` (`mmcblk0p2`) environment block before assembling `emmc.flash`, so the box keeps its real hardware MAC on your LAN.

### Flashing from the Android Root Shell

Because HiNAS writes the whole disk head (`boot` -> `ubuntu` rootfs) cleanly in one shot, you can flash `emmc.flash` directly from the UART root shell using static `busybox`:

```sh
# 1. Compute MD5 of emmc.flash on your PC first (e.g., 3819e3bac67e2514064e4f46344381d6)
# 2. Stop Android userspace services so partitions aren't actively written:
stop

# 3. Write emmc.flash to /dev/block/mmcblk0:
B=/data/local/tmp/b/busybox
$B dd if=/data/local/tmp/emmc.flash of=/dev/block/mmcblk0 bs=1048576 conv=fsync
$B sync
```

### Verify Before Rebooting!

**Do not reboot until you verify the written bytes back from eMMC** (matching the exact byte count `size / 1024`):

```sh
echo 3 > /proc/sys/vm/drop_caches
$B dd if=/dev/block/mmcblk0 bs=1024 count=1472352 2>/dev/null | $B md5sum
```

Once the MD5 matches your source `emmc.flash` file verbatim, trigger a clean reset:

```sh
echo 1 > /proc/sys/kernel/sysrq
echo b > /proc/sysrq-trigger
```

---

## 5. First Boot & Post-Install Cleanup

On first boot, HiNAS automatically:
- Expands/initializes the Ubuntu `mmcblk0p9` filesystem and swapfile (`create swapfile` takes ~1m45s once)
- Starts `ttyAMA0` serial login, SSH (`22`), web panel (`80`), and `ttyd` (`7681`)

Recommended post-install steps on the new Ubuntu 20.04 box:
1. **Change default passwords** (`root` and web panel password) and install your SSH public key.
2. **Disable unnecessary NAS bloatware** (if using the box strictly as a TV Quran/media appliance): services like `aria2`, `transmission`, `alist`, `filebrowser`, `vsftpd`, `nfs`, `linkease`, `frpc`, `vlmcsd`, and `root-passwd-web` can be disabled to save RAM and eMMC wear.
3. **Custom Boot Logo (optional):** `mmcblk0p5` (`/dev/mmcblk0p5`) stores a `LOGO_TABLE` header with a baseline 4:2:0 JPEG starting at offset `0x2000` (and its byte length as a little-endian `uint32` at `0x78`). Wait until `/proc/uptime >= 60s` before starting `/dev/fb0` services (`quran-tv/systemd/hdmi-status.boot-delay.conf`) so early boot does not overwrite your framebuffer.
4. Proceed to the main [`README.md`](../README.md) to set up the Quran player, prayer display, and [`LINUX_VIDEO_OUTPUT.md`](LINUX_VIDEO_OUTPUT.md) for `/dev/fb0` + hardware video decoding.

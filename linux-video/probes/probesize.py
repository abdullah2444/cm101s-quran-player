# usage: probesize.py /dev/x type(hex) nr(hex) [dir=3] [max]
# finds the ioctl argument size(s) the kernel accepts for a command number, using an all-0xff buffer
# (invalid handles, so the driver rejects the request itself) and watching the kernel log for "unsupport".
import fcntl, os, sys
dev, typ, nr = sys.argv[1], int(sys.argv[2], 16), int(sys.argv[3], 16)
d = int(sys.argv[4]) if len(sys.argv) > 4 else 3
mx = int(sys.argv[5]) if len(sys.argv) > 5 else 16380
fd = os.open(dev, os.O_RDWR | os.O_NONBLOCK)
k = os.open("/dev/kmsg", os.O_RDONLY | os.O_NONBLOCK)
os.lseek(k, 0, os.SEEK_END)

def drain():
    out = []
    while True:
        try:
            out.append(os.read(k, 8192).decode("utf-8", "replace"))
        except BlockingIOError:
            return "".join(out)
        except OSError:
            continue

hits = []
for size in range(4, mx + 1, 4):
    cmd = (d << 30) | (size << 16) | (typ << 8) | nr
    drain()
    try:
        fcntl.ioctl(fd, cmd, bytearray(b"\xff" * size), True)
    except OSError:
        pass
    m = drain()
    if "unsupport" not in m.lower():
        hits.append((size, m.strip().split("\n")[-1][-100:] if m.strip() else ""))
print("nr %02x dir %d:" % (nr, d), hits[:6], "(%d hits)" % len(hits))

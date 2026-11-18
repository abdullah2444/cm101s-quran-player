# usage: probe2.py /dev/x type(hex) dir nr(hex) [nr...]
# for each command number, tries every argument size with an all-0xff buffer (invalid handle) and
# groups the results by (errno, last kernel log line); the odd one out is the size the driver accepts.
import fcntl, os, sys
dev, typ, d = sys.argv[1], int(sys.argv[2], 16), int(sys.argv[3])
fd = os.open(dev, os.O_RDWR | os.O_NONBLOCK)
k = os.open("/dev/kmsg", os.O_RDONLY | os.O_NONBLOCK)
os.lseek(k, 0, os.SEEK_END)

def drain():
    o = b""
    while True:
        try:
            o += os.read(k, 8192)
        except BlockingIOError:
            return o.decode("utf-8", "replace")
        except OSError:
            continue

for nrs in sys.argv[4:]:
    nr = int(nrs, 16)
    res = {}
    for size in range(4, 8193, 4):
        cmd = (d << 30) | (size << 16) | (typ << 8) | nr
        drain()
        try:
            fcntl.ioctl(fd, cmd, bytearray(b"\xff" * size), True); r = 0
        except OSError as e:
            r = e.errno
        m = drain().strip().split("\n")[-1]
        m = m[m.find("]:") + 2:][:70] if "]:" in m else m[-70:]
        key = (r, "" if "unsupport" in m.lower() else m)
        res.setdefault(key, []).append(size)
    odd = sorted(res.items(), key=lambda kv: len(kv[1]))
    print("nr %02x:" % nr, [(kk, v[:4]) for kk, v in odd[:3]])

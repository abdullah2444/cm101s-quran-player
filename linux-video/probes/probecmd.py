# usage: probecmd.py /dev/x < "NAME hexcmd" lines ; calls each cmd with an all-0xff buffer
# (invalid handles) and reports which ones the kernel says it does not support
import fcntl, os, sys, subprocess
fd = os.open(sys.argv[1], os.O_RDWR | os.O_NONBLOCK)
for line in sys.stdin:
    name, h = line.split(); cmd = int(h, 16)
    size = (cmd >> 16) & 0x3fff
    subprocess.run(["dmesg", "-c"], stdout=subprocess.DEVNULL)
    try:
        if size: fcntl.ioctl(fd, cmd, bytearray(b"\xff" * size), True)
        else: fcntl.ioctl(fd, cmd, 0)
        r = "ok"
    except OSError as e:
        r = "errno%d" % e.errno
    msg = subprocess.run(["dmesg"], capture_output=True, text=True).stdout
    print("%-40s %s size=%3d %s %s" % (name, h, size, r, "UNSUPPORTED" if "unsupport" in msg.lower() else ""))

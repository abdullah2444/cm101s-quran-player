#!/usr/bin/env python3
"""Put the CM101s-2 into real standby (suspend to RAM). The HiSilicon power MCU stays on and
wakes the box when the remote's red power key is pressed (or after an optional timeout).
Usage: box-standby.py [wake_after_seconds]   (default: no timeout)"""
import array, fcntl, os, struct, subprocess, sys, time, urllib.request

POWER_KEY = 0xe718fe01          # red power key: IR bytes 01 fe 18 e7, little-endian
PM_MAGIC, VAL_SIZE = 0x62, 252  # HI_ID_PM; size of C51_PMOC_VAL_S in this kernel
def IOC(d, nr, size): return (d << 30) | (size << 16) | (PM_MAGIC << 8) | nr
GET_WAKEUP, SET_WAKEUP = IOC(2, 0x0C, VAL_SIZE), IOC(1, 0x02, VAL_SIZE)
WAKEUP_READY, GET_WAKEUPMODE = IOC(0, 0x03, 0), IOC(2, 0x0E, 16)
LOG = '/var/log/box-standby.log'

def log(msg):
    with open(LOG, 'a') as f:
        f.write(time.strftime('%Y-%m-%d %H:%M:%S ') + msg + '\n')

def player(act):
    try:
        urllib.request.urlopen('http://127.0.0.1:8790/cmd?act=' + act, timeout=3).read()
    except Exception:
        pass

def main():
    timeout = int(sys.argv[1]) if len(sys.argv) > 1 else 0xffffffff
    fd = os.open('/dev/hi_pm', os.O_RDWR)
    val = array.array('B', bytes(VAL_SIZE))
    fcntl.ioctl(fd, GET_WAKEUP, val, True)
    struct.pack_into('<I', val, 0, 1)                 # one IR key
    struct.pack_into('<I', val, 4, POWER_KEY)         # irlVal[0]
    struct.pack_into('<I', val, 28, 0)                # irhVal[0]
    struct.pack_into('<I', val, 56, timeout)          # wake after N s (0xffffffff = never)
    fcntl.ioctl(fd, SET_WAKEUP, val)
    player('pause')
    subprocess.run(['sync'])
    log('standby (wake key 0x%08x, timeout %s)' % (POWER_KEY, 'none' if timeout == 0xffffffff else '%d s' % timeout))
    fcntl.ioctl(fd, WAKEUP_READY)
    with open('/sys/power/state', 'w') as f:
        f.write('mem')
    # ---- the box sleeps here and continues after waking ----
    # (the chip's wake-reason register always says "remote" here, even for a timer wake,
    #  so no reason is logged; the box has no battery clock, so this time is only right
    #  after the network clock catches up a few seconds later)
    log('woke up')

if __name__ == '__main__':
    main()

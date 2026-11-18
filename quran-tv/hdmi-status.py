#!/usr/bin/env python3
"""HDMI status screen for the CM101s-2 (HiNAS). Draws to /dev/fb0 (hifb).
The HiSilicon layer is only visible while this process keeps /dev/fb0 open."""
import fcntl, struct, mmap, os, re, time, json, math, signal, socket, subprocess, urllib.request
from datetime import datetime, timedelta
from PIL import Image, ImageDraw, ImageFont

W, H = 1920, 1080
INTERVAL = 5
FONT = '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf'
BOLD = '/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf'
BG, CARD, LINE = (15, 18, 24), (26, 31, 41), (42, 48, 59)
TEXT, MUTED = (230, 233, 239), (138, 147, 163)
ACCENT, GREEN, WARN, RED = (79, 140, 255), (47, 179, 107), (224, 160, 48), (224, 80, 80)

SHORT = {'aria2': 'Aria2', 'cron': 'Schedules', 'avahi': 'mDNS', 'samba': 'File sharing',
         'ftp': 'FTP', 'smart': 'Disk health', 'rootreset': 'Root reset (!)', 'frpc': 'FRP tunnels'}

_fonts = {}
def F(size, bold=False):
    k = (size, bold)
    if k not in _fonts:
        _fonts[k] = ImageFont.truetype(BOLD if bold else FONT, size)
    return _fonts[k]

# ---------- framebuffer ----------
def open_fb():
    fd = os.open('/dev/fb0', os.O_RDWR)
    buf = bytearray(160)
    fcntl.ioctl(fd, 0x4600, buf)
    v = list(struct.unpack('40I', buf))
    v[0:8] = [W, H, W, H, 0, 0, 32, 0]
    v[8:20] = [16, 8, 0, 8, 8, 0, 0, 8, 0, 24, 8, 0]   # ARGB8888
    v[21] = 0
    fcntl.ioctl(fd, 0x4601, struct.pack('40I', *v))
    fix = bytearray(80)
    fcntl.ioctl(fd, 0x4602, fix)
    smem, = struct.unpack_from('I', fix, 20)
    stride, = struct.unpack_from('I', fix, 44)
    return fd, mmap.mmap(fd, smem), stride

def to_fb(img, m, stride):
    data = img.convert('RGBA').tobytes('raw', 'BGRA')     # bytes B,G,R,A = ARGB little-endian
    if stride == W * 4:
        m[0:len(data)] = data
    else:
        for y in range(H):
            m[y * stride:y * stride + W * 4] = data[y * W * 4:(y + 1) * W * 4]

# ---------- data ----------
def read(p, default=''):
    try:
        with open(p) as f:
            return f.read()
    except OSError:
        return default

class Stats:
    def __init__(self):
        self.cpu_prev = None
        self.net_prev = None
        self.svc = []
        self.svc_t = 0

    def cpu(self):
        p = [int(x) for x in read('/proc/stat').split('\n')[0].split()[1:]]
        idle, total = p[3] + p[4], sum(p)
        pct = 0.0
        if self.cpu_prev:
            dt = total - self.cpu_prev[1]
            pct = 100.0 * (1 - (idle - self.cpu_prev[0]) / dt) if dt else 0.0
        self.cpu_prev = (idle, total)
        return max(0.0, min(100.0, pct))

    def net(self):
        rx = tx = 0
        for l in read('/proc/net/dev').split('\n')[2:]:
            if ':' in l:
                name, d = l.split(':', 1)
                if name.strip() in ('eth0', 'wlan0'):
                    d = d.split()
                    rx += int(d[0]); tx += int(d[8])
        now = time.time()
        r = (0.0, 0.0)
        if self.net_prev:
            dt = now - self.net_prev[0]
            if dt > 0:
                r = (max(0, rx - self.net_prev[1]) / dt, max(0, tx - self.net_prev[2]) / dt)
        self.net_prev = (now, rx, tx)
        return r

    def services(self):
        if time.time() - self.svc_t > 30:
            self.svc_t = time.time()
            try:
                out = subprocess.run(['/usr/local/sbin/panel-svc', 'list'], capture_output=True,
                                     text=True, timeout=10).stdout
                self.svc = [SHORT.get(p[0], p[1]) for p in (l.split('|') for l in out.splitlines())
                            if len(p) == 5 and p[3] != 'off' and p[0] != 'hdmi']
            except Exception:
                pass
        return self.svc

def temp():
    m = re.search(r'temperature\s*=\s*(-?\d+)', read('/proc/msp/pm_cpu'))
    return int(m.group(1)) if m else None

def mem():
    d = {}
    for l in read('/proc/meminfo').split('\n'):
        p = l.split()
        if len(p) >= 2:
            d[p[0].rstrip(':')] = int(p[1])
    tot = d.get('MemTotal', 1)
    return tot - d.get('MemAvailable', 0), tot

def disks():
    out, seen = [], set()
    for l in read('/proc/mounts').split('\n'):
        p = l.split()
        if len(p) < 2:
            continue
        dev, mnt = p[0], p[1].replace('\\040', ' ')
        if mnt == '/' or (dev.startswith('/dev/sd') or dev.startswith('/dev/mmcblk1')):
            if dev in seen:
                continue
            seen.add(dev)
            try:
                s = os.statvfs(mnt)
            except OSError:
                continue
            tot = s.f_blocks * s.f_frsize
            if tot:
                out.append(('System' if mnt == '/' else os.path.basename(mnt) or mnt,
                            tot - s.f_bfree * s.f_frsize, tot))
    return out[:3]

def ip_addr():
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(('192.0.2.1', 9))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except OSError:
        return 'no network'

def aria2(method, params=None):
    req = urllib.request.Request('http://127.0.0.1:6800/jsonrpc', json.dumps(
        {'jsonrpc': '2.0', 'id': 'tv', 'method': method, 'params': params or []}).encode(),
        {'Content-Type': 'application/json'})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    return json.loads(opener.open(req, timeout=2).read())['result']

def downloads():
    try:
        g = aria2('aria2.getGlobalStat')
        act = aria2('aria2.tellActive', [['totalLength', 'completedLength', 'downloadSpeed', 'files', 'bittorrent']])
    except Exception:
        return None, []
    items = []
    for a in act[:4]:
        name = ''
        bt = a.get('bittorrent') or {}
        if bt.get('info', {}).get('name'):
            name = bt['info']['name']
        elif a.get('files'):
            f = a['files'][0]
            name = os.path.basename(f.get('path') or '') or (f.get('uris') or [{}])[0].get('uri', '')
        tot, done = int(a['totalLength']), int(a['completedLength'])
        items.append((name or 'Starting...', done / tot if tot else 0.0, int(a['downloadSpeed']), tot))
    return g, items

# ---------- prayer times (calculated locally) ----------
# Override via /etc/prayer-location.json {"lat": ..., "lng": ..., "tz": ..., "city": "..."}
# Default: Makkah, Muslim World League (Fajr 18°, Isha 17°), standard (Shafi'i) Asr.
def _load_location():
    loc = {'lat': 21.4225, 'lng': 39.8262, 'tz': 3, 'city': 'Makkah'}
    try:
        d = json.loads(open('/etc/prayer-location.json').read())
        for k in ('lat', 'lng', 'tz', 'city'):
            if k in d:
                loc[k] = d[k]
    except Exception:
        pass
    return loc['lat'], loc['lng'], loc['tz'], loc['city']

LAT, LNG, TZ, CITY = _load_location()
FAJR_ANGLE, ISHA_ANGLE, ASR_FACTOR = 18.0, 17.0, 1
PRAYERS = ['Fajr', 'Sunrise', 'Dhuhr', 'Asr', 'Maghrib', 'Isha']
PRAYER_WINDOW = timedelta(minutes=20)
PRAYER_ACK = '/var/lib/prayer-reminder-ack.json'
IR_LOG = '/run/ir-keys.log'
PRAYER_TOAST = '/run/remote-toast.json'
_pt = {}
_prayer_ack = {'loaded': False, 'record': {}}
_ir = {'initialized': False, 'identity': None, 'offset': 0, 'partial': '', 'last': 0.0}

def _dsin(x): return math.sin(math.radians(x))
def _dcos(x): return math.cos(math.radians(x))

def _sun(jd):
    D = jd - 2451545.0
    g = (357.529 + 0.98560028 * D) % 360
    q = (280.459 + 0.98564736 * D) % 360
    L = (q + 1.915 * _dsin(g) + 0.020 * _dsin(2 * g)) % 360
    e = 23.439 - 0.00000036 * D
    ra = (math.degrees(math.atan2(_dcos(e) * _dsin(L), _dcos(L))) / 15) % 24
    eqt = q / 15 - ra
    decl = math.degrees(math.asin(_dsin(e) * _dsin(L)))
    return decl, eqt

def prayer_times(day):
    """{name: datetime} for the given date."""
    k = day.toordinal()
    if k in _pt:
        return _pt[k]
    y, m, d = day.year, day.month, day.day
    if m <= 2:
        y, m = y - 1, m + 12
    a = y // 100
    jd = int(365.25 * (y + 4716)) + int(30.6001 * (m + 1)) + d + 2 - a + a // 4 - 1524.5 - LNG / 360

    def noon(t):
        return (12 - _sun(jd + t)[1]) % 24

    def angle_time(angle, t, before):
        decl = _sun(jd + t)[0]
        c = (-_dsin(angle) - _dsin(decl) * _dsin(LAT)) / (_dcos(decl) * _dcos(LAT))
        T = math.degrees(math.acos(max(-1, min(1, c)))) / 15
        return noon(t) + (-T if before else T)

    def asr(t):
        decl = _sun(jd + t)[0]
        ang = -math.degrees(math.atan(1 / (ASR_FACTOR + math.tan(math.radians(abs(LAT - decl))))))
        return angle_time(ang, t, False)

    h = [angle_time(FAJR_ANGLE, 5 / 24, True), angle_time(0.833, 6 / 24, True), noon(12 / 24),
         asr(13 / 24), angle_time(0.833, 18 / 24, False), angle_time(ISHA_ANGLE, 18 / 24, False)]
    base = datetime(day.year, day.month, day.day)
    out = {}
    for name, hr in zip(PRAYERS, h):
        hr += TZ - LNG / 15
        out[name] = base + timedelta(minutes=round(hr * 60))
    if len(_pt) >= 3:
        _pt.clear()
    _pt[k] = out
    return out

def next_prayer(now):
    """(name, time) of the next prayer (sunrise is skipped), looking into tomorrow after Isha."""
    for day in (now.date(), now.date() + timedelta(days=1)):
        for name, t in prayer_times(day).items():
            if name != 'Sunrise' and t > now:
                return name, t

def prayer_window(now):
    """The most recent prayer and countdown end, only during its 20-minute reminder."""
    days = [now.date()]
    previous = (now - PRAYER_WINDOW).date()
    if previous != days[0]:
        days.append(previous)       # a reminder can continue across midnight
    active = [(t, name) for day in days for name, t in prayer_times(day).items()
              if name != 'Sunrise' and t <= now < t + PRAYER_WINDOW]
    if active:
        start, name = max(active)
        return name, start + PRAYER_WINDOW
    return None

def prayer_acknowledged(active):
    if not _prayer_ack['loaded']:
        try:
            record = json.loads(read(PRAYER_ACK, '{}'))
            _prayer_ack['record'] = record if isinstance(record, dict) else {}
        except ValueError:
            _prayer_ack['record'] = {}
        _prayer_ack['loaded'] = True
    name, end = active
    record = _prayer_ack['record']
    return record.get('prayer') == name and record.get('start') == (end - PRAYER_WINDOW).isoformat()

def visible_prayer(now):
    """Show a reminder until this particular prayer has been marked as prayed."""
    active = prayer_window(now)
    return None if active and prayer_acknowledged(active) else active

def acknowledge_prayer(when):
    """Remember only the current prayer, so later prayers still get their reminder."""
    active = prayer_window(when)
    if not active or prayer_acknowledged(active):
        return False
    name, end = active
    record = {'prayer': name, 'start': (end - PRAYER_WINDOW).isoformat()}
    try:
        with open(PRAYER_ACK + '.tmp', 'w') as f:
            json.dump(record, f)
        os.replace(PRAYER_ACK + '.tmp', PRAYER_ACK)
    except OSError as e:
        print('prayer acknowledgement error:', e, flush=True)
        return False
    _prayer_ack['record'] = record
    try:
        with open(PRAYER_TOAST + '.prayer.tmp', 'w') as f:
            json.dump({'text': name + ' marked as prayed', 't': time.time()}, f)
        os.replace(PRAYER_TOAST + '.prayer.tmp', PRAYER_TOAST)
    except OSError:
        pass
    return True

def poll_remote_mouse(now, enabled=True):
    """Follow new IR key-down records without competing with the Quran player's IR reader."""
    tick = time.monotonic()
    if tick - _ir['last'] < 0.2:
        return
    _ir['last'] = tick
    try:
        with open(IR_LOG, 'r') as f:
            st = os.fstat(f.fileno())
            identity = (st.st_dev, st.st_ino)
            if not _ir['initialized']:
                _ir.update(initialized=True, identity=identity, offset=st.st_size)
                return                    # never replay keys from before the screen started
            if identity != _ir['identity'] or st.st_size < _ir['offset']:
                _ir.update(identity=identity, offset=0, partial='')
            f.seek(_ir['offset'])
            data = f.read()
            _ir['offset'] = f.tell()
    except OSError:
        _ir['initialized'] = True         # the first new log may be created by the next key
        return
    lines = (_ir['partial'] + data).split('\n')
    _ir['partial'] = lines.pop()
    if not enabled:
        return                             # a key pressed while the screen is off is not a confirmation
    for line in lines:
        fields = line.split()
        if len(fields) < 3 or fields[1:3] != ['01fe04fb', 'mouse']:
            continue
        try:
            h, m, s = (int(x) for x in fields[0].split(':'))
            when = now.replace(hour=h, minute=m, second=s, microsecond=0)
        except ValueError:
            continue
        if when - now > timedelta(hours=12):
            when -= timedelta(days=1)      # a press just before midnight
        age = (now - when).total_seconds()
        if -2 <= age <= 15:               # ignore old or implausibly future log entries
            acknowledge_prayer(when)

def prayer_countdown(now, end):
    seconds = max(0, math.ceil((end - now).total_seconds()))
    return f'{seconds // 60:02d}:{seconds % 60:02d}'

def until(now, t):
    m = int((t - now).total_seconds() // 60) + 1
    return f'{m // 60}h {m % 60:02d}m' if m >= 60 else f'{m} min'

# ---------- formatting ----------
def size(n):
    for u in ('B', 'KB', 'MB', 'GB', 'TB'):
        if n < 1024 or u == 'TB':
            return f'{n:.0f} {u}' if u in ('B', 'KB') else f'{n:.1f} {u}'
        n /= 1024

def rate(n):
    return size(n) + '/s'

def uptime():
    s = int(float(read('/proc/uptime', '0').split()[0]))
    d, h, m = s // 86400, s % 86400 // 3600, s % 3600 // 60
    return f'{d}d {h}h' if d else (f'{h}h {m}m' if h else f'{m}m')

def fit(d, text, font, width):
    if d.textsize(text, font=font)[0] <= width:
        return text
    while text and d.textsize(text + '…', font=font)[0] > width:
        text = text[:-1]
    return text + '…'

# ---------- drawing ----------
def card(d, x, y, w, h):
    d.rounded_rectangle([x, y, x + w, y + h], 18, fill=CARD) if hasattr(d, 'rounded_rectangle') \
        else d.rectangle([x, y, x + w, y + h], fill=CARD)

def bar(d, x, y, w, frac, color, h=14):
    d.rectangle([x, y, x + w, y + h], fill=LINE)
    if frac > 0:
        d.rectangle([x, y, x + max(h, int(w * min(frac, 1))), y + h], fill=color)

def level(frac):
    return GREEN if frac < 0.7 else (WARN if frac < 0.9 else RED)

def render(st):
    img = Image.new('RGB', (W, H), BG)
    d = ImageDraw.Draw(img)
    M = 80
    now = datetime.now()

    # header
    time12(d, M, 50, now, 150, 52, TEXT)
    d.text((M + 6, 230), now.strftime('%A, %-d %B %Y'), font=F(40), fill=MUTED)
    right = [('HiNAS', F(60, True), TEXT), (f'CM101s-2  ·  {ip_addr()}', F(36), ACCENT),
             (f'Up {uptime()}', F(32), MUTED)]
    y = 66
    for t, f, c in right:
        tw = d.textsize(t, font=f)[0]
        d.text((W - M - tw, y), t, font=f, fill=c)
        y += f.size + 26

    # four cards
    top, ch, gap = 300, 214, 30
    cw = (W - 2 * M - 3 * gap) // 4
    xs = [M + i * (cw + gap) for i in range(4)]
    for x in xs:
        card(d, x, top, cw, ch)
    pad = 32

    cpu, t = st.cpu(), temp()
    d.text((xs[0] + pad, top + 22), 'CPU', font=F(30), fill=MUTED)
    d.text((xs[0] + pad, top + 62), f'{cpu:.0f}%', font=F(76, True), fill=TEXT)
    if t is not None:
        tc = GREEN if t < 75 else (WARN if t < 90 else RED)
        d.text((xs[0] + pad, top + 158), f'{t}°C', font=F(32, True), fill=tc)
    fm = re.search(r'freq\s*=\s*(\d+)', read('/proc/msp/pm_cpu'))
    lt = f'{int(fm.group(1)) / 1e6:.1f} GHz' if fm else ''
    d.text((xs[0] + cw - pad - d.textsize(lt, font=F(28))[0], top + 162), lt, font=F(28), fill=MUTED)

    used, tot = mem()
    d.text((xs[1] + pad, top + 22), 'Memory', font=F(30), fill=MUTED)
    d.text((xs[1] + pad, top + 62), f'{used / tot * 100:.0f}%', font=F(76, True), fill=TEXT)
    d.text((xs[1] + pad, top + 146), f'{used // 1024} of {tot // 1024} MB', font=F(28), fill=MUTED)
    bar(d, xs[1] + pad, top + 186, cw - 2 * pad, used / tot, level(used / tot))

    ds = disks()
    d.text((xs[2] + pad, top + 22), 'Storage', font=F(30), fill=MUTED)
    if ds:
        name, u, tt = ds[0]
        d.text((xs[2] + pad, top + 62), f'{u / tt * 100:.0f}%', font=F(76, True), fill=TEXT)
        d.text((xs[2] + pad, top + 146), fit(d, f'{size(tt - u)} free · {name}', F(28), cw - 2 * pad),
               font=F(28), fill=MUTED)
        bar(d, xs[2] + pad, top + 186, cw - 2 * pad, u / tt, level(u / tt))

    rx, tx = st.net()
    d.text((xs[3] + pad, top + 22), 'Network', font=F(30), fill=MUTED)
    d.text((xs[3] + pad, top + 72), '↓ ' + rate(rx), font=F(46, True), fill=TEXT)
    d.text((xs[3] + pad, top + 138), '↑ ' + rate(tx), font=F(46, True), fill=MUTED)

    # prayer times strip
    py = top + ch + gap
    prayer_strip(d, M, py, W - 2 * M, 150, now)

    # apps panel: which remote key opens each app
    py, ph = py + 150 + gap, 300
    dw = 1100
    card(d, M, py, dw, ph)
    d.text((M + pad, py + 26), 'Apps', font=F(34, True), fill=TEXT)
    hint = 'press the key on the remote'
    d.text((M + dw - pad - d.textsize(hint, font=F(26))[0], py + 32), hint, font=F(26), fill=MUTED)
    tg = 16
    tw_ = (dw - 2 * pad - (len(APPS) - 1) * tg) // len(APPS)
    for i, (name, icon, key, sub) in enumerate(APPS):
        tx, ty, th = M + pad + i * (tw_ + tg), py + 92, ph - 92 - pad
        d.rectangle([tx, ty, tx + tw_, ty + th], fill=(36, 44, 58))
        ic = app_icon(icon, 58)
        if ic:
            img.paste(ic, (tx + 14, ty + 20), ic)
        d.text((tx + 82, ty + 22), name, font=F(28, True), fill=TEXT)
        sc = MUTED
        if name == 'Quran':
            qn = quran_now()
            if qn:
                sub = ('▶ ' if qn['state'] == 'playing' else '❚❚ ') + qn.get('name', '')
                sc = GREEN if qn['state'] == 'playing' else WARN
        d.text((tx + 82, ty + 60), fit(d, sub, F(20), tw_ - 90), font=F(20), fill=sc)
        key_chip(d, tx + 14, ty + th - 50, key)

    # services panel
    sx = M + dw + gap
    sw2 = W - M - sx
    card(d, sx, py, sw2, ph)
    d.text((sx + pad, py + 26), 'Running', font=F(34, True), fill=TEXT)
    names = ['SSH', 'Web panel'] + st.services()
    col_w = (sw2 - 2 * pad) // 2
    for i, n in enumerate(names[:10]):
        cx = sx + pad + (i // 5) * col_w
        cy = py + 90 + (i % 5) * 40
        d.ellipse([cx, cy + 9, cx + 14, cy + 23], fill=GREEN)
        d.text((cx + 28, cy), fit(d, re.sub(r'\s*\(.*\)', '', n), F(27), col_w - 40), font=F(27), fill=TEXT)
    return img

APPS = [('Quran', 'quran', 'Green', 'open the Quran'),
        ('Videos', 'videos', 'Set', 'choose a video'),
        ('AirPlay', 'videos', 'Menu', 'iPhone screen')]
_icons = {}

def app_icon(name, size):
    k = (name, size)
    if k not in _icons:
        try:
            _icons[k] = Image.open(f'/var/www/html/img/png/{name}.png').convert('RGBA').resize((size, size), Image.LANCZOS)
        except Exception:
            _icons[k] = None
    return _icons[k]

def key_chip(d, x, y, key):
    """The remote key: a coloured dot for Red/Green, an outlined label for the others."""
    f = F(28, True)
    if key in ('Red', 'Green'):
        d.ellipse([x, y + 3, x + 30, y + 33], fill=RED if key == 'Red' else GREEN)
        d.text((x + 42, y), key, font=f, fill=TEXT)
    else:
        w = d.textsize(key.upper(), font=F(24, True))[0]
        d.rectangle([x, y, x + w + 28, y + 36], outline=TEXT, width=3)
        d.text((x + 14, y + 4), key.upper(), font=F(24, True), fill=TEXT)
        d.text((x + w + 42, y), 'key', font=F(28), fill=MUTED)

def mouse_hint(d, x, y, size=22, color=MUTED):
    """A small drawing of the remote's mouse-logo key, followed by its purpose."""
    font = F(size)
    d.text((x, y), 'Press', font=font, fill=color)
    mx = x + d.textsize('Press', font=font)[0] + 9
    mw, mh = max(12, int(size * 0.75)), max(18, int(size * 1.05))
    d.line([mx + mw // 2, y, mx + mw // 2, y + 3], fill=color, width=2)
    d.ellipse([mx, y + 3, mx + mw, y + 3 + mh], outline=color, width=2)
    d.line([mx + mw // 2, y + 4, mx + mw // 2, y + 3 + mh // 2], fill=color, width=2)
    d.text((mx + mw + 9, y), 'to close', font=font, fill=color)

def prayer_strip(d, x, y, w, h, now):
    """Full-width prayer times with a reminder after the prayer begins."""
    card(d, x, y, w, h)
    nxt, nt = next_prayer(now)
    active = visible_prayer(now)
    times = prayer_times(now.date())
    title = 'Time to pray' if active else 'Prayer times'
    message = (f'{active[0]}  {prayer_countdown(now, active[1])} left' if active
               else f'{nxt} in {until(now, nt)}')
    d.text((x + 32, y + 26), title, font=F(30), fill=WARN if active else MUTED)
    fs = 40
    while fs > 30 and d.textsize(message, font=F(fs, True))[0] > 448:
        fs -= 2
    d.text((x + 32, y + 66), message, font=F(fs, True), fill=WARN if active else TEXT)
    if active:
        mouse_hint(d, x + 32, y + 112)
    else:
        d.text((x + 32, y + 116), CITY, font=F(22), fill=MUTED)
    left = 480
    colw = (w - left - 20) // 6
    for i, name in enumerate(PRAYERS):
        cx = x + left + i * colw
        t = times[name]
        is_next = name == nxt and t.date() == nt.date()
        is_active = bool(active and name == active[0] and t == active[1] - PRAYER_WINDOW)
        if is_active:
            d.rectangle([cx + 6, y + 18, cx + colw - 6, y + h - 18], fill=(68, 49, 20))
        elif is_next:
            d.rectangle([cx + 6, y + 18, cx + colw - 6, y + h - 18], fill=(36, 56, 92))
        passed = t <= now and not is_active
        nc = WARN if is_active else (ACCENT if is_next else MUTED)
        tc = TEXT if not passed else (100, 108, 122)
        nw = d.textsize(name, font=F(28))[0]
        d.text((cx + (colw - nw) // 2, y + 34), name, font=F(28), fill=nc)
        tw = time12(d, None, 0, t, 50, 22, tc)
        time12(d, cx + (colw - tw) // 2, y + 74, t, 50, 22, tc)

def time12(d, x, y, t, big, small, color):
    """Draw '5:18' with a small 'AM' after it; returns the total width (x=None only measures)."""
    hm = f'{t.hour % 12 or 12}:{t.minute:02d}'
    ap = 'AM' if t.hour < 12 else 'PM'
    fb, fs = F(big, True), F(small, True)
    w1 = d.textsize(hm, font=fb)[0]
    gap = max(4, small // 4)
    if x is not None:
        d.text((x, y), hm, font=fb, fill=color)
        d.text((x + w1 + gap, y + int((big - small) * 0.85)), ap, font=fs, fill=color)
    return w1 + gap + d.textsize(ap, font=fs)[0]

# ---------- Quran mode ----------
QBASE = '/home/ubuntu/quran'
QNOW = '/run/quran-now.json'
LEAD_MS = 700                 # highlight this much early: covers the redraw time and TV delay
INK = (236, 231, 218)
HL = (44, 70, 118)
_q = {}
_pages = {}

def qdata():
    if 'd' not in _q:
        _q['d'] = json.load(open(QBASE + '/quran.json'))
        _q['tim'] = {int(k): v['v'] for k, v in _q['d']['timings'].items()}
    return _q['d']

def quran_now():
    try:
        s = json.loads(read(QNOW, '{}'))
    except ValueError:
        return None
    if s.get('state') in ('playing', 'paused') and time.time() - s.get('t', 0) < 5:
        if s['state'] == 'playing':     # catch up to now, and look ahead by the time a redraw takes
            s['pos_ms'] += int((time.time() - s['t']) * 1000) + LEAD_MS
        return s
    return None

def page_mask(p):
    if p not in _pages:
        if len(_pages) >= 4:
            _pages.pop(next(iter(_pages)))
        _pages[p] = Image.open(f'{QBASE}/pages/{p:03d}.png').convert('L')
    return _pages[p]

BH = 112                      # status bar height in Quran mode
VH = H - BH                   # visible page height

def current_verse(s, ms):
    qdata()
    cur = (1, 0, 1)
    for verse, a, b in _q['tim'].get(s, []):
        if a <= ms:
            cur = (verse, a, b)
        else:
            break
    return cur

def quran_view(q):
    """Which verse, which page and how far down the (zoomed) page to show."""
    qd = qdata()
    verse, a, b = current_verse(q['surah'], q['pos_ms'])
    key = f"{q['surah']}:{verse}"
    frac = min(max((q['pos_ms'] - a) / max(b - a, 1), 0), 1)     # progress through this verse
    first = qd['verse_page'].get(key, 1)
    parts = [(p, qd['boxes'].get(str(p), {}).get(key)) for p in (first, first + 1)]
    parts = [(p, r) for p, r in parts if r]
    if not parts:
        return verse, key, first, 0
    # verse split over two pages: switch page in proportion to its lines
    nlines = sum(len(r) for _, r in parts)
    page, rects = parts[0]
    done = len(parts[0][1]) / nlines
    sub = frac / done if done else 0
    if len(parts) > 1 and frac >= done:
        page, rects = parts[1]
        sub = (frac - done) / (1 - done)
    PH = qd['page_size'][1]
    y0 = min(r[1] for r in rects) - 30
    y1 = max(r[3] for r in rects) + 30
    if y1 - y0 <= VH:
        oy = (y0 + y1) / 2 - VH / 2
    else:                                   # long verse: scroll along while it is recited
        oy = y0 + sub * (y1 - y0 - VH)
    oy = int(min(max(oy, 0), PH - VH))
    oy -= oy % 34                           # only move in steps (fewer redraws, steadier picture)
    return verse, key, page, oy

def prayer_column(d, x0, x1, y, now):
    """Compact prayer list for the left margin of the Quran view."""
    nxt, nt = next_prayer(now)
    active = visible_prayer(now)
    times = prayer_times(now.date())
    # the margin is narrow, so the countdown gets its own line (it used to run into the page)
    d.text((x0, y), 'Time to pray' if active else 'Next prayer', font=F(30), fill=WARN if active else MUTED)
    d.text((x0, y + 40), active[0] if active else nxt, font=F(46, True), fill=TEXT)
    message = prayer_countdown(now, active[1]) + ' left' if active else 'in ' + until(now, nt)
    d.text((x0, y + 100), message, font=F(36, True), fill=WARN if active else ACCENT)
    if active:
        mouse_hint(d, x0, y + 145)
    y += 184
    for name in PRAYERS:
        t = times[name]
        is_next = name == nxt and t.date() == nt.date()
        is_active = bool(active and name == active[0] and t == active[1] - PRAYER_WINDOW)
        if is_active:
            d.rectangle([x0 - 14, y - 12, x1 + 14, y + 56], fill=(68, 49, 20))
        elif is_next:
            d.rectangle([x0 - 14, y - 12, x1 + 14, y + 56], fill=(36, 56, 92))
        passed = t <= now and not is_active
        c = WARN if is_active else (ACCENT if is_next else ((96, 104, 118) if passed else TEXT))
        tw = time12(d, None, 0, t, 42, 20, c)
        bold, fs = is_next or is_active, 36
        available = max(0, x1 - x0 - tw - 16)
        while fs > 24 and d.textsize(name, font=F(fs, bold))[0] > available:
            fs -= 2
        d.text((x0, y + 2 + (36 - fs) // 2), fit(d, name, F(fs, bold), available), font=F(fs, bold), fill=c)
        time12(d, x1 - tw, y - 2, t, 42, 20, c)
        y += 80

_grid = {}

def page_grid(page):
    """Line centres, line pitch and text column edges of a page, from all its verse boxes."""
    if page not in _grid:
        rects = [r for v in qdata()['boxes'].get(str(page), {}).values() for r in v]
        cs = []
        for c in sorted((r[1] + r[3]) / 2 for r in rects):
            if cs and c - cs[-1][-1] < 50:
                cs[-1].append(c)
            else:
                cs.append([c])
        centres = [sum(c) / len(c) for c in cs]
        pitch = (centres[-1] - centres[0]) / (len(centres) - 1) if len(centres) > 1 else 130
        # text column = outermost verse edges (lines are justified, so every line
        # reaches both edges; a page may have no single verse spanning a whole line)
        left = min((r[0] for r in rects), default=0)
        right = max((r[2] for r in rects), default=0)
        if len(_grid) > 8:
            _grid.clear()
        _grid[page] = (centres, pitch, left - 14, right + 14)
    return _grid[page]

def verse_bands(page, key):
    """One clean band per line: equal height, lines touching, and lines that continue
    run to the column edge (the verse starts on the right and ends on the left)."""
    rects = sorted(qdata()['boxes'].get(str(page), {}).get(key, []), key=lambda r: r[1])
    if not rects:
        return []
    centres, pitch, L, R = page_grid(page)
    cs = [min(centres, key=lambda k: abs(k - (r[1] + r[3]) / 2)) for r in rects]
    n = len(rects)
    out = []
    for i, (x0, y0, x1, y1) in enumerate(rects):
        # neighbouring lines meet half-way, so there is never a gap or an overlap
        top = (cs[i - 1] + cs[i]) / 2 if i > 0 else cs[i] - pitch / 2
        bot = (cs[i] + cs[i + 1]) / 2 if i < n - 1 else cs[i] + pitch / 2
        bx0 = L if i < n - 1 or x0 - L < 40 else x0 - 6
        bx1 = R if i > 0 or R - x1 < 40 else x1 + 6
        out.append((int(bx0), int(round(top)), int(bx1), int(round(bot))))
    return out

def render_quran(st, q, view):
    qd = qdata()
    verse, key, page, oy = view
    s = q['surah']
    PW, PH = qd['page_size']
    img = Image.new('RGB', (W, H), BG)
    d = ImageDraw.Draw(img)

    # zoomed single page (drawn first, the bar goes on top)
    x = 200                       # page sits left of centre: verse/page number on the left, prayers on the right
    d.rectangle([x - 20, BH, x + PW + 20, H], fill=CARD)
    for x0, y0, x1, y1 in verse_bands(page, key):
        d.rectangle([x + x0, BH + y0 - oy, x + x1, BH + y1 - oy], fill=HL)
    mask = page_mask(page).crop((0, oy, PW, oy + VH))
    img.paste(INK, (x, BH, x + PW, BH + VH), mask)
    # scroll indicator + page number in the side margins
    sx = x + PW + 34
    d.rectangle([sx, BH + 40, sx + 6, H - 40], fill=LINE)
    span = H - 80 - BH
    d.rectangle([sx, BH + 40 + int(span * oy / PH), sx + 6, BH + 40 + int(span * (oy + VH) / PH)], fill=ACCENT)
    d.text((x - 20 - 22 - d.textsize('Page', font=F(26))[0], H - 120), 'Page', font=F(26), fill=MUTED)
    pn = str(page)
    d.text((x - 20 - 22 - d.textsize(pn, font=F(44, True))[0], H - 86), pn, font=F(44, True), fill=TEXT)
    vt = f'{s}:{verse}'
    vs = 44
    while vs > 26 and d.textsize(vt, font=F(vs, True))[0] > x - 20 - 22 - 16:
        vs -= 2                                     # long verse numbers (26:227) shrink to fit the margin
    d.text((x - 20 - 22 - d.textsize(vt, font=F(vs, True))[0], BH + 40), vt, font=F(vs, True), fill=ACCENT)
    prayer_column(d, sx + 40, W - 36, BH + 40, datetime.now())

    # slim status bar
    d.rectangle([0, 0, W, BH], fill=CARD)
    now = datetime.now()
    lx = max(290, 48 + time12(d, 48, 18, now, 66, 26, TEXT) + 44)
    cpu, t = st.cpu(), temp()
    used, tot = mem()
    rx, _ = st.net()
    line1 = f'{ip_addr()}  ·  up {uptime()}'
    line2 = f'CPU {cpu:.0f}%' + (f'  {t}°C' if t is not None else '') + \
            f'   ·   RAM {used / tot * 100:.0f}%   ·   ↓ {rate(rx)}'
    d.text((lx, 22), line1, font=F(28), fill=ACCENT)
    d.text((lx, 62), line2, font=F(26), fill=MUTED)
    title = f'{s}. {q["name"]}'
    sub = f'{q["meaning"]}  ·  verse {verse} of {q["verses"]}' + ('  ·  paused' if q['state'] == 'paused' else '')
    tw = d.textsize(title, font=F(44, True))[0]
    d.text((W - 48 - tw, 12), title, font=F(44, True), fill=TEXT)
    sw = d.textsize(sub, font=F(26))[0]
    d.text((W - 48 - sw, 68), sub, font=F(26), fill=WARN if q['state'] == 'paused' else MUTED)
    total = q.get('length_ms') or 1
    d.rectangle([0, BH - 4, W, BH], fill=LINE)
    d.rectangle([0, BH - 4, int(W * min(q['pos_ms'] / total, 1)), BH], fill=ACCENT)
    return img


def remote_toast():
    """Text of the last remote-control action, for 2.5 s after it happened."""
    try:
        t = json.loads(read('/run/remote-toast.json', '{}'))
    except ValueError:
        return None
    return t.get('text') if time.time() - t.get('t', 0) < 2.5 else None

def volume_img(v):
    """Volume as a bar of 20 blocks with a speaker sign, no number."""
    n, bw, gap, h = 20, 30, 8, 110
    w = 48 + 60 + 32 + n * (bw + gap) - gap + 48
    t = Image.new('RGB', (w, h), (36, 56, 92))
    d = ImageDraw.Draw(t)
    d.rectangle([0, h - 6, w, h], fill=ACCENT)
    cy = (h - 6) // 2
    d.rectangle([48, cy - 12, 64, cy + 12], fill=TEXT)                      # speaker
    d.polygon([(64, cy - 12), (88, cy - 28), (88, cy + 28), (64, cy + 12)], fill=TEXT)
    for i, r in enumerate((14, 26)):                                         # sound waves
        if v > i * 50:
            d.arc([88 - r, cy - r, 88 + r + 4, cy + r], -50, 50, fill=TEXT, width=4)
    on = round(min(99, max(0, v)) / 99 * n)
    x0 = 48 + 60 + 32
    for i in range(n):
        bh = 18 + i * 2                                                      # blocks grow a little
        x = x0 + i * (bw + gap)
        d.rectangle([x, cy + 30 - bh * 1.4, x + bw, cy + 30], fill=ACCENT if i < on else (60, 78, 110))
    return t, (W - w) // 2, H - h - 70

def toast_img(text):
    if text.startswith('Volume '):
        try:
            return volume_img(int(text.split()[1]))
        except ValueError:
            pass
    f = F(48, True)
    tw, th = ImageDraw.Draw(Image.new('RGB', (1, 1))).textsize(text, font=f)
    w, h = tw + 96, 110
    t = Image.new('RGB', (w, h), (36, 56, 92))
    d = ImageDraw.Draw(t)
    d.rectangle([0, h - 6, w, h], fill=ACCENT)
    d.text((48, (h - 6 - th) // 2 - 4), text, font=f, fill=TEXT)
    return t, (W - w) // 2, H - h - 70

def draw_toast(img, text):
    t, x, y = toast_img(text)
    img.paste(t, (x, y))

def prayer_banner(now, active):
    """A small overlay for video views, or a transparent tile to clear it."""
    img = Image.new('RGBA', (460, 100), (0, 0, 0, 0))
    if active:
        d = ImageDraw.Draw(img)
        d.rectangle([0, 0, img.width, img.height], fill=CARD + (235,))
        d.rectangle([0, img.height - 5, img.width, img.height], fill=WARN + (255,))
        d.text((20, 8), f'{active[0]}  ·  Time to pray', font=F(25, True), fill=WARN + (255,))
        label = prayer_countdown(now, active[1]) + ' left'
        d.text((20, 40), label, font=F(38, True), fill=TEXT + (255,))
        mouse_hint(d, 20 + d.textsize(label, font=F(38, True))[0] + 16, 57, 16, MUTED + (255,))
    return img

# ---- video from the NAS: vplay draws it on the video layer, so this layer stays see-through
# except a progress bar for a few seconds after a key (and while paused) and the toasts.
VNOW = '/run/video.json'

def video_now():
    try:
        v = json.loads(read(VNOW, '{}'))
    except ValueError:
        return None
    return v if v.get('on') and time.time() - v.get('t', 0) < 6 else None

def clock_txt(ms):
    s = max(0, int(ms // 1000))
    h, s = divmod(s, 3600)
    m, s = divmod(s, 60)
    return f'{h}:{m:02}:{s:02}' if h else f'{m}:{s:02}'

def video_bar(v):
    w, h = W - 160, 150
    img = Image.new('RGBA', (w, h), (15, 18, 24, 230))
    d = ImageDraw.Draw(img)
    d.rectangle([0, h - 6, w, h], fill=ACCENT)
    if v.get('paused'):                                   # pause sign
        d.rectangle([40, 34, 54, 82], fill=TEXT); d.rectangle([66, 34, 80, 82], fill=TEXT)
    else:                                                 # play sign
        d.polygon([(42, 32), (82, 58), (42, 84)], fill=TEXT)
    pos, dur = v.get('pos', 0), v.get('dur', 0)
    times = f'{clock_txt(pos)} / {clock_txt(dur)}' if dur else clock_txt(pos)
    tw = d.textsize(times, font=F(34))[0]
    d.text((w - 40 - tw, 30), times, font=F(34), fill=MUTED)
    d.text((112, 24), fit(d, v.get('title', ''), F(40, True), w - 112 - tw - 80), font=F(40, True), fill=TEXT)
    bar(d, 112, 100, w - 112 - 40, pos / dur if dur else 0, ACCENT, h=12)
    return img, 80, H - h - 40

VMENU = '/run/video-menu.json'

def video_menu():
    try:
        mn = json.loads(read(VMENU, '{}'))
    except ValueError:
        return None
    return mn if mn.get('on') and time.time() - mn.get('t', 0) < 180 else None

def render_menu(mn, now=None):
    """Video chooser on the TV (remote: ▲▼ move, OK open/play, Back up)."""
    img = Image.new('RGB', (W, H), BG)
    d = ImageDraw.Draw(img)
    M = 80
    ic = app_icon('videos', 92)
    if ic:
        img.paste(ic, (M, 44), ic)
    d.text((M + 120, 46), 'Videos', font=F(58, True), fill=TEXT)
    now = now or datetime.now()
    tw = time12(d, None, 0, now, 56, 22, TEXT)
    time12(d, W - M - tw, 52, now, 56, 22, TEXT)
    d.text((M + 122, 120), fit(d, mn.get('path', 'Media'), F(30), W - 2 * M - 122 - tw - 40), font=F(30), fill=MUTED)
    items, sel = mn.get('items', []), mn.get('sel', 0)
    rows, rh, top = 9, 86, 196
    start = max(0, min(sel - rows // 2, len(items) - rows))
    for i in range(start, min(len(items), start + rows)):
        it, y = items[i], top + (i - start) * rh
        h = rh - 12
        on = i == sel
        d.rectangle([M, y, W - M, y + h], fill=(36, 56, 92) if on else CARD)
        if on:
            d.rectangle([M, y, M + 8, y + h], fill=ACCENT)
        cx, cy = M + 52, y + h // 2
        col = ACCENT if it['type'] == 'continue' else (TEXT if on else MUTED)
        if it['type'] == 'dir':
            d.rectangle([cx - 20, cy - 12, cx + 22, cy + 16], fill=col)
            d.rectangle([cx - 20, cy - 18, cx - 2, cy - 10], fill=col)
        else:
            d.polygon([(cx - 12, cy - 18), (cx + 18, cy), (cx - 12, cy + 18)], fill=col)
        right = ''
        if it.get('saved'):
            right = 'from ' + clock_txt(it['saved'])
        rw = d.textsize(right, font=F(28))[0] if right else 0
        f = F(36, True) if on or it['type'] == 'continue' else F(36)
        d.text((M + 100, cy - 22), fit(d, it['name'], f, W - 2 * M - 100 - rw - 60), font=f,
               fill=ACCENT if it['type'] == 'continue' and not on else TEXT)
        if right:
            d.text((W - M - 30 - rw, cy - 16), right, font=F(28), fill=WARN)
    if not items:
        d.text((M + 20, top + 30), 'No videos here', font=F(36), fill=MUTED)
    elif len(items) > rows:
        pos = f'{sel + 1} / {len(items)}'
        d.text((W - M - d.textsize(pos, font=F(26))[0], H - 62), pos, font=F(26), fill=MUTED)
    d.text((M, H - 62), '▲ ▼  choose      ◀ ▶  page      OK  open / play      Back  go up      Set  close',
           font=F(26), fill=MUTED)
    active = visible_prayer(now)
    if active:
        banner = prayer_banner(now, active)
        img.paste(banner, ((W - banner.width) // 2, 14), banner)
    return img

def video_frame(m, stride, v, tt, show):
    """Bottom part of the screen: transparent, with the bar and/or a toast on it."""
    top = H - 360
    reg = Image.new('RGBA', (W, H - top), (0, 0, 0, 0))
    if show:
        b, bx, by = video_bar(v)
        reg.paste(b, (bx, by - top))
    if tt:
        t, x, y = toast_img(tt)
        if show:
            y = H - 150 - 40 - 24 - t.height              # above the bar
        reg.paste(t, (x, y - top))
    blit(m, stride, reg, 0, top)

def blit(m, stride, img, x, y):
    """Write one small picture straight into the framebuffer (much cheaper than the whole screen)."""
    x0, y0 = max(x, 0), max(y, 0)
    x1, y1 = min(x + img.width, W), min(y + img.height, H)
    if x1 <= x0 or y1 <= y0:
        return
    img = img.crop((x0 - x, y0 - y, x1 - x, y1 - y))
    data = img.convert('RGBA').tobytes('raw', 'BGRA')
    rw = (x1 - x0) * 4
    for row in range(y1 - y0):
        o = (y0 + row) * stride + x0 * 4
        m[o:o + rw] = data[row * rw:(row + 1) * rw]

def video_prayer_banner(m, stride, now, active):
    banner = prayer_banner(now, active)
    blit(m, stride, banner, W - banner.width - 40, 24)

def airplay_now():
    try:
        state = json.loads(read('/run/airplay.json', '{}'))
        return state if state.get('on') and time.time() - state.get('t', 0) < 6 else None
    except (ValueError, TypeError):
        return None

def airplay_waiting():
    img = Image.new('RGBA', (W, H), BG + (255,))
    d = ImageDraw.Draw(img)
    password = ''
    try:
        with open('/etc/cm101s-airplay.conf') as f:
            for line in f:
                fields = line.split()
                if len(fields) == 2 and fields[0] == 'pw':
                    password = fields[1]
                    break
    except OSError:
        pass
    for text, y, size, color in [
            ('AirPlay', H // 2 - 140, 70, TEXT),
            ('Choose CM101s AirPlay on your iPhone', H // 2 - 30, 38, MUTED),
            ('Password: ' + password, H // 2 + 35, 38, TEXT),
            ('Press Back to stop', H // 2 + 105, 30, MUTED)]:
        font = F(size)
        width, _ = d.textsize(text, font=font)
        d.text(((W - width) // 2, y), text, font=font, fill=color)
    return img

def main():
    os.nice(10)
    fd, m, stride = open_fb()
    st = Stats()
    st.cpu(); st.net()
    running = [True]
    signal.signal(signal.SIGTERM, lambda *a: running.__setitem__(0, False))
    shift = [(0, 0), (6, 4), (-6, 4), (-6, -4), (6, -4)]
    started = time.time()
    last_draw, last_key = 0, None
    blank = False
    while running[0]:
        screen_off = os.path.exists('/run/screen-off')
        poll_remote_mouse(datetime.now(), not screen_off)
        if screen_off:                             # picture off from the remote: draw nothing, idle
            if not blank:
                m[0:W * H * 4] = bytes(1) * (W * H * 4)
                blank, last_key = True, None
            time.sleep(1)
            continue
        blank = False
        nowt = time.time()
        now = datetime.fromtimestamp(nowt)
        active = visible_prayer(now)
        prayer_key = (active[0], int(nowt // 5)) if active else None
        ap = airplay_now()
        if ap:
            tt = remote_toast()
            key = ('airplay', bool(ap.get('streaming')), tt, prayer_key)
            if key != last_key:
                if ap.get('streaming'):
                    m[0:W * H * 4] = bytes(W * H * 4)
                    video_prayer_banner(m, stride, now, active)
                else:
                    img = airplay_waiting()
                    if tt:
                        draw_toast(img, tt)
                    to_fb(img, m, stride)
                last_draw, last_key = nowt, key
            time.sleep(0.2)
            continue
        v = video_now()
        if v:                                          # a video plays: see-through, bar now and then
            tt = remote_toast()
            show = v.get('paused') or nowt - v.get('shown', 0) < 4 or v.get('pos', 0) < 4000
            key = ('v', show, int(v.get('pos', 0) // 1000) if show else 0, v.get('paused'), v.get('title'), tt,
                   prayer_key)
            if key != last_key:
                if not last_key or last_key[0] != 'v':
                    m[0:W * H * 4] = bytes(W * H * 4)
                video_frame(m, stride, v, tt, show)
                video_prayer_banner(m, stride, now, active)
                last_draw, last_key = nowt, key
            time.sleep(0.2)
            continue
        mn = video_menu()
        if mn:                                         # video chooser
            tt = remote_toast()
            key = ('m', mn.get('path'), mn.get('sel'), len(mn.get('items', [])), tt, int(nowt // 60), prayer_key)
            if key != last_key:
                try:
                    img = render_menu(mn, now)
                    if tt:
                        draw_toast(img, tt)
                    to_fb(img, m, stride)
                except Exception as e:
                    print('menu render error:', e, flush=True)
                last_draw, last_key = nowt, key
            time.sleep(0.04)
            continue
        q = quran_now()
        qv = None if os.path.exists('/run/quran-hidden') else q   # Back/Home: main page, audio goes on
        n = int((nowt - started) // 600)              # move a few pixels every 10 min (screen burn-in)
        dx, dy = shift[n % len(shift)]
        if qv:
            view = quran_view(qv)
            key = ('q', qv['surah'], qv['state']) + view
        else:
            key = ('s',)
        tt = remote_toast()
        key = key + (tt, dx, dy, prayer_key)
        if key != last_key or nowt - last_draw >= INTERVAL:
            try:
                img = render_quran(st, qv, view) if qv else render(st)
            except Exception as e:          # never leave the TV blank because of bad data
                print('render error:', e, flush=True)
                img = render(st)
            if tt:
                draw_toast(img, tt)
            if dx or dy:
                moved = Image.new(img.mode, (W, H), BG + (255,) if img.mode == 'RGBA' else BG)
                moved.paste(img, (dx, dy))
                img = moved
            to_fb(img, m, stride)
            last_draw, last_key = nowt, key
        time.sleep(0.05)
    m[0:W * H * 4] = b'\x00' * (W * H * 4)
    os.close(fd)

if __name__ == '__main__':
    main()

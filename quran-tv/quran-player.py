#!/usr/bin/env python3
"""Quran player for the CM101s-2: plays Mishary Alafasy surah MP3s on the AUX jack via mpg123,
exposes a small control API on 127.0.0.1:8790 and publishes the position to /run/quran-now.json
for the HDMI screen."""
import json, math, os, re, select, subprocess, threading, time, bisect, secrets
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

BASE = '/home/ubuntu/quran'
AUDIO = BASE + '/audio/%03d.mp3'
SAVE = BASE + '/player-state.json'
NOW = '/run/quran-now.json'
RATE = 44100
Q = json.load(open(BASE + '/quran.json'))
CH = Q['chapters']
TIM = {int(k): v['v'] for k, v in Q['timings'].items()}

lock = threading.RLock()
st = {'surah': 1, 'pos_ms': 0, 'state': 'stopped', 'volume': 30, 'repeat': 'next'}
try:
    _saved = json.load(open(SAVE))
    st.update(_saved)
    st['state'] = 'stopped'
    if 'vol_db' not in _saved and st['volume'] > 0:   # old straight-percent scale -> same loudness on the new scale
        st['volume'] = max(1, min(99, round(99 + 40 * math.log10(st['volume'] / 99))))
except Exception:
    pass

def set_volume(vol):
    """The HISI-AIAO mixer ignores writes (amixer reports success but the level never changes),
    so the volume is applied in software by mpg123: 0-99 on the panel = 0-100 % of full level."""
    v = max(0, min(99, int(vol)))
    # ears hear loudness in steps of dB, not in percent: each 5 steps = 2.5 dB,
    # so every press sounds like the same change (99 = full, 1 = very quiet, 0 = off)
    pct = 0 if v == 0 else 100 * 10 ** ((v - 99) * 0.5 / 20)
    mpg.send('VOLUME %.2f' % pct)
    video_send('v %d' % v)               # the video player uses the same volume

class Mpg:
    def __init__(self):
        self.p = subprocess.Popen(['mpg123', '-R', '-o', 'alsa', '-a', 'plughw:0,0', '--no-gapless'],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, bufsize=1)
        self.send('SILENCE')
        threading.Thread(target=self.reader, daemon=True).start()

    def send(self, cmd):
        if cmd.startswith(('LOAD', 'PAUSE', 'JUMP')):
            silence_off()                # mpg123 is about to open the sound output
        try:
            self.p.stdin.write(cmd + '\n'); self.p.stdin.flush()
        except Exception:
            pass

    def reader(self):
        for line in self.p.stdout:
            line = line.strip()
            if line == '@P 0':          # track finished (or stopped)
                with lock:
                    if st['state'] == 'playing' and time.time() - clock['t0'] > 3:
                        threading.Thread(target=track_end, daemon=True).start()

# Position comes from the wall clock: mpg123 reports decoded (not played) samples, which runs ahead.
LATENCY_MS = 350          # time from LOAD/JUMP until sound actually starts
clock = {'base': 0, 't0': 0.0}

def clock_set(ms):
    clock['base'] = ms; clock['t0'] = time.time() + LATENCY_MS / 1000

def clock_pos():
    if st['state'] == 'playing':
        return max(clock['base'], clock['base'] + int((time.time() - clock['t0']) * 1000))
    return st['pos_ms']
# The AUX amplifier hisses whenever nothing is sent to it, so while the Quran is not
# playing, pure silence is fed to the output. mpg123 needs the output to itself,
# so the silence is stopped just before mpg123 opens it.
PCM_STATUS = '/proc/asound/card0/pcm0p/sub0/status'
silent = {'p': None}

def silence_off():
    p = silent['p']
    silent['p'] = None
    if p and p.poll() is None:
        p.terminate()
        try:
            p.wait(timeout=2)
        except subprocess.TimeoutExpired:
            p.kill(); p.wait()

def silence_keeper():
    while True:
        time.sleep(0.3)
        with lock:
            if st['state'] == 'playing' or (silent['p'] and silent['p'].poll() is None) or \
                    ('airplay' in globals() and airplay['audio_claimed']) or \
                    ('video_busy' in globals() and video_busy()):
                continue
            try:
                closed = open(PCM_STATUS).read().strip() == 'closed'
            except OSError:
                closed = False
            if closed:
                silent['p'] = subprocess.Popen(
                    ['aplay', '-q', '-D', 'plughw:0,0', '-f', 'S16_LE', '-r', '48000', '-c', '2', '-t', 'raw', '/dev/zero'],
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

mpg = Mpg()
threading.Thread(target=silence_keeper, daemon=True).start()

def verse_at(s, ms):
    v = TIM.get(s, [])
    i = bisect.bisect_right([x[1] for x in v], ms) - 1
    return v[max(i, 0)][0] if v else 1

def verse_ms(s, verse):
    for v, a, b in TIM.get(s, []):
        if v == verse:
            return a
    return 0

def play(s, start_ms=0):
    s = max(1, min(114, int(s)))
    with lock:
        st.update(surah=s, pos_ms=start_ms, state='playing')
        clock_set(start_ms)
        mpg.send('LOAD ' + AUDIO % s)
        set_volume(0 if muted['vol'] is not None else st['volume'])
        if start_ms > 1000:
            mpg.send('JUMP %.2fs' % (start_ms / 1000))
    save()

def track_end():
    with lock:
        s = st['surah']
        if st['repeat'] == 'surah':
            nxt = s
        else:
            nxt = s + 1 if s < 114 else 1      # after the last surah start again from Al-Fatiha
    play(nxt)

def cmd(q):
    with vlock:
        return cmd_locked(q)

def cmd_locked(q):
    a = q.get('act', [''])[0]
    if a in ('play', 'resume', 'toggle', 'next', 'prev', 'nextverse', 'prevverse'):
        airplay_stop()
    if a in ('play', 'resume', 'toggle', 'next', 'prev', 'nextverse', 'prevverse') and video_busy():
        video_stop()                     # the Quran needs the sound output back
    if a in ('play', 'resume', 'next', 'prev'):
        quran_hide(False)
    with lock:
        if a == 'play':
            s = int(q.get('surah', [st['surah']])[0])
            verse = int(q.get('verse', ['1'])[0])
            play(s, verse_ms(s, verse) if verse > 1 else 0)
        elif a == 'resume':
            if st['state'] == 'paused':
                mpg.send('PAUSE'); st['state'] = 'playing'; clock_set(st['pos_ms'])
            elif st['state'] == 'stopped':
                play(st['surah'], st['pos_ms'])
        elif a == 'pause':
            if st['state'] == 'playing':
                st['pos_ms'] = clock_pos(); mpg.send('PAUSE'); st['state'] = 'paused'
        elif a == 'toggle':
            return cmd({'act': ['pause' if st['state'] == 'playing' else 'resume']})
        elif a == 'stop':
            st['pos_ms'] = clock_pos(); mpg.send('STOP'); st['state'] = 'stopped'
        elif a in ('next', 'prev'):
            play((st['surah'] - 1 + (1 if a == 'next' else -1)) % 114 + 1)
        elif a in ('nextverse', 'prevverse'):
            st['pos_ms'] = clock_pos()
            v = verse_at(st['surah'], st['pos_ms']) + (1 if a == 'nextverse' else -1)
            v = max(1, min(CH[str(st['surah'])]['verses'], v))
            ms = verse_ms(st['surah'], v)
            if st['state'] == 'stopped':
                play(st['surah'], ms)
            else:
                mpg.send('JUMP %.2fs' % (ms / 1000)); st['pos_ms'] = ms; clock_set(ms)
        elif a == 'volume':
            st['volume'] = max(0, min(99, int(q.get('v', [st['volume']])[0])))
            set_volume(st['volume'])
        elif a == 'repeat':
            st['repeat'] = 'surah' if q.get('v', [''])[0] == 'surah' else 'next'
    save()
    return status()

def status():
    with lock:
        s = st['surah']
        return dict(st, verse=verse_at(s, st['pos_ms']), verses=CH[str(s)]['verses'],
                    name=CH[str(s)]['name'], meaning=CH[str(s)]['meaning'],
                    length_ms=TIM[s][-1][2] if TIM.get(s) else 0)

def save():
    with lock:
        d = {k: st[k] for k in ('surah', 'pos_ms', 'volume', 'repeat')}
        d['vol_db'] = 1
    try:
        with open(SAVE + '.tmp', 'w') as f:
            json.dump(d, f)
        os.replace(SAVE + '.tmp', SAVE)
    except OSError:
        pass

def publisher():
    n = 0
    while True:
        time.sleep(0.25)
        with lock:
            if st['state'] == 'playing':
                st['pos_ms'] = clock_pos()
        s = status()
        s['t'] = time.time()
        try:
            with open(NOW + '.tmp', 'w') as f:
                json.dump(s, f)
            os.replace(NOW + '.tmp', NOW)
        except OSError:
            pass
        if 'airplay_active' in globals() and airplay_active():
            airplay_publish()
        n += 1
        if n % 40 == 0 and s['state'] == 'playing':
            save()

# ---------- IR remote (/dev/hi_ir) ----------
# Each read gives a 56-byte record: bytes 8-11 = key code, 16-47 = protocol name, 48 = 0 down / 1 hold / 2 up.
TOAST = '/run/remote-toast.json'
KEYS = {
    '01fe46b9': 'up', '01fe16e9': 'down', '01fe47b8': 'left', '01fe15ea': 'right', '01fe55aa': 'ok',
    '01fe40bf': 'back', '01fe06f9': 'home', '01fe17e8': 'menu',
    '01fe1ae5': 'red', '01fe3cc3': 'green', '01fe14eb': 'voldown', '01fe10ef': 'volup',
    '01fef20d': 'set', '01fef30c': 'tvin', '01fef40b': 'voldown', '01fef50a': 'volup',
    '01fe08f7': 'mute', '01fe4bb4': 'del', '01fe04fb': 'mouse', '01fe18e7': 'power_red', '01fef10e': 'power',
    '01fe0cf3': '0', '01fe54ab': '1', '01fe48b7': '2', '01fe07f8': '3', '01fe50af': '4',
    '01fe12ed': '5', '01fe11ee': '6', '01fe4cb3': '7', '01fe0ef1': '8', '01fe0df2': '9',
}
digits = {'buf': '', 'gen': 0}
muted = {'vol': None}
_gate = {'key': None, 'down': 0.0, 'last': 0.0}

def key_gate(k, hold):
    """One press = one step. The remote repeats a held key about 9 times a second and can
    send a press twice, so: ignore a repeated press of the same key within 0.25 s, and only
    auto-repeat after the key has been held 0.6 s, at most 4 steps a second."""
    now = time.time()
    g = _gate
    if not hold:
        if k == g['key'] and now - g['last'] < 0.25:
            g['last'] = now
            return False
        g.update(key=k, down=now, last=now)
        return True
    if k != g['key'] or now - g['last'] > 0.4:
        g.update(key=k, down=now, last=now)  # the press itself was not received: count this as the press
        return True
    if now - g['down'] < 0.6 or now - g['last'] < 0.25:
        return False
    g['last'] = now
    return True

def toast(text):
    try:
        with open(TOAST + '.tmp', 'w') as f:
            json.dump({'text': text, 't': time.time()}, f)
        os.replace(TOAST + '.tmp', TOAST)
    except OSError:
        pass

def digits_done(gen, wait=1.8):
    time.sleep(wait)
    with lock:
        if gen != digits['gen'] or not digits['buf']:
            return
        n = int(digits['buf']); digits['buf'] = ''
    if 1 <= n <= 114:
        cmd({'act': ['play'], 'surah': [str(n)]})
        toast(f'{n}. {CH[str(n)]["name"]}')
    else:
        toast(f'There is no surah {n}')


SCREEN_OFF = '/run/screen-off'      # exists = TV picture off (hdmi-status idles); Quran keeps playing

def screen_off(off):
    try:
        if off:
            open(SCREEN_OFF, 'w').close()
        elif os.path.exists(SCREEN_OFF):
            os.remove(SCREEN_OFF)
    except OSError:
        pass

QURAN_HIDDEN = '/run/quran-hidden'  # exists = Quran plays on in the background, TV shows the main page

def quran_hidden():
    return os.path.exists(QURAN_HIDDEN)

def quran_hide(hide):
    try:
        if hide:
            open(QURAN_HIDDEN, 'w').close()
        elif os.path.exists(QURAN_HIDDEN):
            os.remove(QURAN_HIDDEN)
    except OSError:
        pass

def quran_stop():
    """Stop the Quran but remember the place (Green / OK carries on from there)."""
    with lock:
        if st['state'] != 'stopped':
            if st['state'] == 'playing':
                st['pos_ms'] = clock_pos()
            mpg.send('STOP'); st['state'] = 'stopped'
    save()


# ---------- videos from the NAS (/usr/local/bin/vplay: hardware decoder + AUX sound) ----------
VIDEO_ROOT = os.environ.get('VIDEO_ROOT', '/mnt/media')  # local/NAS media folder
VIDEO_NOW = '/run/video.json'        # read by hdmi-status (video on the TV, progress bar)
VIDEO_SAVE = '/var/lib/video-player.json'
VIDEO_EXT = ('.mp4', '.m4v', '.mkv', '.mov', '.ts')
video = {'p': None, 'busy': False, 'rel': '', 'pos': 0, 'dur': 0, 'paused': False, 'err': '', 'gen': 0, 'shown': 0.0}
vlock = threading.RLock()
try:
    vsave = json.load(open(VIDEO_SAVE))
except Exception:
    vsave = {}
vsave.setdefault('pos', {})

def natural(s):
    return [int(x) if x.isdigit() else x.lower() for x in re.split(r'(\d+)', s)]

def video_path(rel):
    p = os.path.realpath(os.path.join(VIDEO_ROOT, rel.lstrip('/')))
    if p != VIDEO_ROOT and not p.startswith(VIDEO_ROOT + '/'):
        raise ValueError('outside the media folder')
    return p

def video_ls(rel):
    p = video_path(rel)
    dirs, files = [], []
    for e in os.scandir(p):
        if e.name.startswith(('.', '@', '#')):
            continue
        if e.is_dir():
            dirs.append(e.name)
        elif e.name.lower().endswith(VIDEO_EXT) and not e.name.endswith('.part.mp4'):
            files.append(e.name)
    rel = os.path.relpath(p, VIDEO_ROOT)
    rel = '' if rel == '.' else rel
    return {'dir': rel, 'dirs': sorted(dirs, key=natural), 'files': sorted(files, key=natural),
            'saved': {f: vsave['pos'].get(os.path.join(rel, f), 0) for f in files}}

def video_neighbour(rel, step):
    d, f = os.path.split(rel)
    try:
        files = video_ls(d)['files']
        i = files.index(f) + step
    except (OSError, ValueError):
        return None
    return os.path.join(d, files[i]) if 0 <= i < len(files) else None

def video_title(rel):
    return os.path.splitext(os.path.basename(rel))[0]

def video_busy():
    return video['busy'] or video['p'] is not None

def video_send(c):
    p = video['p']
    if p:
        try:
            p.stdin.write(c + '\n'); p.stdin.flush()
        except Exception:
            pass

def video_save():
    try:
        pos = vsave['pos']
        while len(pos) > 200:
            pos.pop(next(iter(pos)))
        with open(VIDEO_SAVE + '.tmp', 'w') as f:
            json.dump(vsave, f)
        os.replace(VIDEO_SAVE + '.tmp', VIDEO_SAVE)
    except OSError:
        pass

def video_status():
    on = video['p'] is not None
    return {'on': on, 'rel': video['rel'] if on else vsave.get('last', ''),
            'title': video_title(video['rel'] if on else vsave.get('last', '')),
            'folder': os.path.basename(os.path.dirname(video['rel'] if on else vsave.get('last', ''))),
            'pos': video['pos'] if on else vsave['pos'].get(vsave.get('last', ''), 0),
            'dur': video['dur'], 'paused': video['paused'], 'err': video['err'],
            'shown': video['shown'], 'volume': st['volume'], 't': time.time()}

def video_publish():
    try:
        with open(VIDEO_NOW + '.tmp', 'w') as f:
            json.dump(video_status(), f)
        os.replace(VIDEO_NOW + '.tmp', VIDEO_NOW)
    except OSError:
        pass

def video_remember():
    if video['rel']:
        pos = video['pos']
        if video['dur'] and pos > video['dur'] - 15000:
            vsave['pos'].pop(video['rel'], None)          # watched to the end
        else:
            vsave['pos'].pop(video['rel'], None)
            vsave['pos'][video['rel']] = max(0, pos - 3000)
        vsave['last'] = video['rel']
        video_save()

def video_stop(remember=True):
    with vlock:
        p = video['p']
        if not p:
            return
        video['gen'] += 1                 # its reader thread lets go
        if remember:
            video_remember()
        try:
            p.stdin.write('q\n'); p.stdin.flush()
            p.wait(timeout=5)
        except Exception:
            p.kill(); p.wait()
        video.update(p=None, busy=False)
        video_publish()

# AirPlay uses the same decoder, display and ALSA device as the other players.
# vlock serializes transitions between these modes.
AIRPLAY_NOW = '/run/airplay.json'
AIRPLAY_CONFIG = '/etc/cm101s-airplay.conf'
AIRPLAY_BIN = '/usr/local/lib/cm101s-airplay/uxplay'
AIRPLAY_PLUGINS = '/usr/local/lib/cm101s-airplay/gstreamer'
airplay = {'p': None, 'starting': False, 'streaming': False, 'audio_claimed': False, 'error': ''}
airplay_publish_lock = threading.Lock()

def airplay_active():
    p = airplay['p']
    return airplay['starting'] or (p is not None and p.poll() is None)

def airplay_password():
    try:
        with open(AIRPLAY_CONFIG) as f:
            for line in f:
                parts = line.split()
                if len(parts) == 2 and parts[0] == 'pw':
                    return parts[1]
    except OSError:
        pass
    return ''

def airplay_status():
    return {'on': airplay_active(), 'streaming': bool(airplay['streaming']),
            'name': 'CM101s AirPlay', 'password': airplay_password(),
            'error': airplay['error'], 't': time.time()}

def airplay_publish():
    try:
        with airplay_publish_lock:
            with open(AIRPLAY_NOW + '.tmp', 'w') as f:
                # The TV needs only state; do not put the password in /run.
                state = airplay_status()
                state.pop('password', None)
                json.dump(state, f)
            os.replace(AIRPLAY_NOW + '.tmp', AIRPLAY_NOW)
    except OSError:
        pass

def airplay_reader(p):
    for line in p.stdout:
        if airplay['p'] is not p:
            continue
        if 'Begin streaming to GStreamer video pipeline' in line:
            airplay['streaming'] = True
            airplay_publish()
        elif 'raop_rtp_mirror->running is no longer true' in line:
            airplay['streaming'] = False
            airplay['audio_claimed'] = False
            airplay_publish()
        elif 'ERROR' in line and 'eos proc timeout' not in line:
            # Do not retain client addresses or other private stream metadata.
            airplay['error'] = line.strip()[:180]
    code = p.wait()
    with vlock:
        if airplay['p'] is p:
            airplay.update(p=None, starting=False, streaming=False, audio_claimed=False)
            if code and not airplay['error']:
                airplay['error'] = 'AirPlay receiver stopped unexpectedly.'
            airplay_publish()

def airplay_stop():
    with vlock:
        p = airplay['p']
        if p is not None:
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    p.kill(); p.wait()
            airplay['p'] = None
        airplay.update(starting=False, streaming=False, audio_claimed=False)
        airplay_publish()

def airplay_start():
    with vlock:
        if airplay_active():
            return
        if not airplay_password() or not os.path.isfile(AIRPLAY_BIN):
            raise RuntimeError('AirPlay is not installed or configured.')
        airplay.update(starting=True, streaming=False, audio_claimed=False, error='')
        airplay_publish()
        try:
            video_stop()
            menu_close()
            quran_stop()
            screen_off(False)
            env = dict(os.environ, CM101S_HISI_TEST='1', GST_PLUGIN_PATH=AIRPLAY_PLUGINS,
                       CM101S_AUDIO_CLAIM='1',
                       GST_REGISTRY='/var/cache/cm101s-airplay-gst-registry.bin')
            env.pop('CM101S_AUDIO_TEST', None)
            args = ['/usr/bin/stdbuf', '-oL', '-eL', AIRPLAY_BIN,
                    '-rc', AIRPLAY_CONFIG, '-n', 'CM101s AirPlay', '-nh',
                    '-s', '1280x720@30', '-vs', 'fakesink', '-vsync', 'no',
                    '-as', 'alsasink device=plughw:0,0', '-vol', '0.25']
            p = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                 stdin=subprocess.DEVNULL, text=True, bufsize=1, env=env)
            airplay.update(p=p, starting=False)
            threading.Thread(target=airplay_reader, args=(p,), daemon=True).start()
            airplay_publish()
        except Exception:
            airplay.update(p=None, starting=False, streaming=False, audio_claimed=False)
            airplay_publish()
            raise

def airplay_claim_audio():
    with vlock:
        with lock:
            if not airplay_active() or st['state'] != 'stopped':
                raise RuntimeError('AirPlay cannot claim the audio device.')
            airplay['audio_claimed'] = True
            silence_off()
        for _ in range(30):
            try:
                if open(PCM_STATUS).read().strip() == 'closed':
                    return
            except OSError:
                pass
            time.sleep(0.1)
        airplay['audio_claimed'] = False
        raise RuntimeError('The audio device is still in use.')

def airplay_api(q):
    try:
        action = q.get('act', [''])[0]
        if action == 'start':
            airplay_start()
        elif action == 'stop':
            airplay_stop()
        elif action == 'claim_audio':
            airplay_claim_audio()
            return {'claimed': True}
        elif action:
            raise ValueError('Unknown AirPlay action.')
    except Exception as e:
        airplay['error'] = str(e)
        airplay_publish()
    return airplay_status()


# ---- video chooser on the TV (Set key) ----
VMENU = '/run/video-menu.json'       # read by hdmi-status
menu = {'on': False, 'dir': '', 'items': [], 'sel': 0, 'tk': 0.0}

def menu_publish():
    path = ' / '.join(['Media'] + (menu['dir'].split('/') if menu['dir'] else []))
    try:
        with open(VMENU + '.tmp', 'w') as f:
            json.dump({'on': menu['on'], 'path': path, 'items': menu['items'], 'sel': menu['sel'],
                       't': time.time()}, f)
        os.replace(VMENU + '.tmp', VMENU)
    except OSError:
        pass

def menu_load(d, select=None):
    try:
        ls = video_ls(d)
    except Exception:
        toast('Cannot open the NAS')
        return False
    items = []
    last = vsave.get('last')
    if last and (ls['dir'] == '' or os.path.dirname(last) == ls['dir']):
        items.append({'type': 'continue', 'name': 'Continue: ' + video_title(last), 'rel': last,
                      'saved': vsave['pos'].get(last, 0)})
    j = lambda n: os.path.join(ls['dir'], n) if ls['dir'] else n
    items += [{'type': 'dir', 'name': n, 'rel': j(n)} for n in ls['dirs']]
    items += [{'type': 'file', 'name': os.path.splitext(f)[0], 'rel': j(f), 'saved': ls['saved'].get(f, 0)}
              for f in ls['files']]
    menu.update(on=True, dir=ls['dir'], items=items, sel=0, tk=time.time())
    for i, it in enumerate(items):
        if select and it['type'] != 'continue' and it['rel'] == select:
            menu['sel'] = i
            break
    menu_publish()
    return True

def menu_open():
    quran_stop()                          # the Videos app needs the sound: Quran off (place kept)
    screen_off(False)
    last = vsave.get('last')
    if not (last and menu_load(os.path.dirname(last), select=last)):
        menu_load('')

def menu_close():
    if menu['on']:
        menu['on'] = False
        menu_publish()

def menu_key(k):
    """Remote while the video chooser shows. Returns True when the key was used."""
    menu['tk'] = time.time()
    items, n = menu['items'], len(menu['items'])
    if k in ('up', 'down'):
        if n:
            menu['sel'] = (menu['sel'] + (1 if k == 'down' else -1)) % n
            menu_publish()
    elif k in ('left', 'right'):
        if n:
            menu['sel'] = max(0, min(n - 1, menu['sel'] + (8 if k == 'right' else -8)))
            menu_publish()
    elif k in ('ok', 'green'):
        if n:
            it = items[menu['sel']]
            if it['type'] == 'dir':
                menu_load(it['rel'])
            else:
                menu_close()
                toast(video_title(it['rel'])[:60])
                threading.Thread(target=video_start, args=(it['rel'],), daemon=True).start()
    elif k == 'back':
        if menu['dir']:
            menu_load(os.path.dirname(menu['dir']), select=menu['dir'])
        else:
            menu_close()
    elif k in ('home', 'set', 'power'):
        menu_close()
    elif k in ('volup', 'voldown', 'mute'):
        return False
    return True

def video_start(rel, start_ms=None):
    path = video_path(rel)
    if not os.path.isfile(path):
        raise ValueError('no such video')
    menu_close()
    with vlock:
        airplay_stop()
        video_stop()
        video['busy'] = True
        with lock:                        # the Quran stops (it can carry on later from the same place)
            if st['state'] != 'stopped':
                if st['state'] == 'playing':
                    st['pos_ms'] = clock_pos()
                mpg.send('STOP'); st['state'] = 'stopped'
        save()
        screen_off(False)
        silence_off()
        for _ in range(40):              # wait until the sound output is free
            try:
                pcm_free = open(PCM_STATUS).read().strip() == 'closed'
            except OSError:
                pcm_free = True
            if pcm_free:
                break
            time.sleep(0.1)
        if start_ms is None:
            start_ms = vsave['pos'].get(rel, 0)
        vol = 0 if muted['vol'] is not None else st['volume']
        p = subprocess.Popen(['/usr/local/bin/vplay', path, '%.1f' % (start_ms / 1000), str(vol)],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                             text=True, bufsize=1)
        video['gen'] += 1
        video.update(p=p, busy=False, rel=rel, pos=start_ms, dur=0, paused=False, err='', shown=time.time())
        vsave['last'] = rel
        video_save()
        threading.Thread(target=video_reader, args=(p, video['gen']), daemon=True).start()
        video_publish()

def video_reader(p, gen):
    ended, n = False, 0
    for line in p.stdout:
        a = line.split()
        if not a or gen != video['gen']:
            continue
        if a[0] == 'T' and len(a) >= 4:
            video['pos'], video['dur'], video['paused'] = int(a[1]), int(a[2]), a[3] == '1'
            video_publish()
            n += 1
            if n % 60 == 0:
                video_remember()          # every 30 s, in case the power goes
        elif a[0] == 'E':
            ended = True
        elif a[0] == 'ERR':
            video['err'] = line[4:].strip()
            toast(video['err'][:60])
    p.wait()
    with vlock:
        if gen != video['gen']:
            return                        # stopped on purpose
        rel = video['rel']
        if ended:
            video['pos'] = video['dur']
        video_remember()
        video.update(p=None)
        nxt = video_neighbour(rel, 1) if ended else None
        if nxt:
            toast(video_title(nxt)[:60])
            try:
                video_start(nxt, 0)       # next episode in the same folder
                return
            except Exception as e:
                print('video next error:', e, flush=True)
        video_publish()

def video_key(k):
    """Remote while a video plays. Returns True when the key was used."""
    video['shown'] = time.time()          # shows the progress bar on the TV for a few seconds
    if k in ('ok', 'green'):
        video_send('p')
        video['paused'] = not video['paused']
        video_publish()
    elif k in ('left', 'right'):
        t = max(0, video['pos'] + (10000 if k == 'right' else -10000))
        if video['dur']:
            t = min(t, max(0, video['dur'] - 2000))
        video_send('s %.1f' % (t / 1000))
        video['pos'] = t
        video_publish()
    elif k in ('up', 'down'):
        nxt = video_neighbour(video['rel'], 1 if k == 'down' else -1)
        if nxt:
            toast(video_title(nxt)[:60])
            threading.Thread(target=video_start, args=(nxt, 0), daemon=True).start()
        else:
            toast('Last episode' if k == 'down' else 'First episode')
    elif k == 'back':
        video_stop()
        menu_open()                       # back to the list, on this episode
    elif k in ('home', 'power'):
        video_stop(); toast('Stopped')
    elif k in ('volup', 'voldown', 'mute'):
        return False                      # shared volume, handled below
    return True

def video_api(q):
    g = lambda k, d='': q.get(k, [d])[0]
    try:
        if 'ls' in q:
            return dict(video_ls(g('ls')), video=video_status())
        if 'play' in q:
            start = g('start')
            threading.Thread(target=video_start, args=(g('play'), int(start) if start.isdigit() else None),
                             daemon=True).start()
            time.sleep(0.2)
        a = g('act')
        if a == 'stop':
            video_stop()
        elif a in ('toggle', 'seek', 'next', 'prev') and video['p']:
            video_key({'toggle': 'ok', 'next': 'down', 'prev': 'up'}.get(a, ''))
            if a == 'seek' and g('t').replace('.', '', 1).isdigit():
                video_send('s ' + g('t')); video['pos'] = int(float(g('t')) * 1000); video_publish()
        elif a == 'resume' and not video['p'] and vsave.get('last'):
            threading.Thread(target=video_start, args=(vsave['last'],), daemon=True).start()
            time.sleep(0.2)
        return {'video': video_status()}
    except Exception as e:
        return {'error': str(e), 'video': video_status()}

power = {'busy': False, 'woke': 0.0}

def standby():
    """Red power key: stop everything (video, Quran, menu), then real standby
    (box-standby.py). The same key wakes the box; nothing starts again by itself."""
    airplay_stop()
    video_stop()
    menu_close()
    quran_stop()
    quran_hide(False)
    toast('Turning off…')
    time.sleep(1.2)                          # let the TV show it
    try:
        subprocess.run(['/usr/local/bin/box-standby.py'], timeout=24 * 3600)   # returns after wake-up
    except Exception as e:
        print('standby error:', e, flush=True)
    power['woke'] = time.time()
    power['busy'] = False

def remote_key(k, hold):
    if airplay_active() and not hold:
        if k in ('back', 'home', 'power'):
            airplay_stop(); toast('AirPlay stopped'); return
        if k in ('red', 'tvin', 'green', 'ok', 'set', 'power_red'):
            airplay_stop()
    if menu['on'] and time.time() - menu['tk'] > 180:
        menu_close()                      # the TV stops showing an untouched list after 3 min
    if hold and k not in ('volup', 'voldown') and not (video['p'] and k in ('left', 'right')) \
            and not (menu['on'] and k in ('up', 'down')):
        return
    if (k == 'menu' and not hold and not power['busy'] and not menu['on'] and
            not video_busy() and
            (st['state'] == 'stopped' or quran_hidden())):
        try:
            airplay_start()
        except Exception as e:
            airplay['error'] = str(e)
            airplay_publish()
            toast('AirPlay could not start')
        return
    if video['p'] and k != 'power_red' and not power['busy']:
        if video_key(k):
            return
    elif menu['on'] and k != 'power_red' and not power['busy']:
        if menu_key(k):
            return
    elif k == 'set' and not hold and not power['busy']:     # Set: video chooser
        menu_open()
        return
    if k == 'power_red':
        # the press that wakes the box also arrives here: ignore it
        if not power['busy'] and time.time() - power['woke'] > 8:
            power['busy'] = True
            threading.Thread(target=standby, daemon=True).start()
        return
    if power['busy']:
        return
    if k == 'power':                    # second power button: TV picture off/on, audio untouched
        if os.path.exists(SCREEN_OFF):
            screen_off(False); toast('Screen on')
        else:
            screen_off(True)
        return
    # Quran keys only work while the Quran shows on the TV. Elsewhere (main page)
    # only app keys and volume work; OK plays/pauses the Quran, Green opens the Quran screen.
    if st['state'] == 'stopped' or quran_hidden():
        if k == 'ok' and not digits['buf'] and not video_busy():
            s = cmd({'act': ['toggle']})
            toast('Playing' if s['state'] == 'playing' else 'Paused')
            return
        if k == 'green' and not digits['buf'] and not video_busy():
            if st['state'] != 'playing':
                cmd({'act': ['resume']})
            quran_hide(False); toast('Quran')
            return
        if k not in ('volup', 'voldown', 'mute'):
            return
    if k.isdigit():
        with lock:
            digits['buf'] = (digits['buf'] + k)[-3:]
            digits['gen'] += 1
            gen, buf = digits['gen'], digits['buf']
        toast(f'Surah {buf}_')
        threading.Thread(target=digits_done, args=(gen,), daemon=True).start()
        return
    if k == 'del':
        with lock:
            digits['buf'] = digits['buf'][:-1]; digits['gen'] += 1
            gen, buf = digits['gen'], digits['buf']
        toast(f'Surah {buf}_' if buf else 'Cancelled')
        if buf:
            threading.Thread(target=digits_done, args=(gen,), daemon=True).start()
        return
    if k == 'ok' and digits['buf']:                 # OK confirms a typed surah number at once
        with lock:
            digits['gen'] += 1
            gen = digits['gen']
        digits_done(gen, 0)
        return
    if k in ('volup', 'voldown'):
        v = max(0, min(99, st['volume'] + (5 if k == 'volup' else -5)))
        muted['vol'] = None
        cmd({'act': ['volume'], 'v': [str(v)]})
        toast(f'Volume {v}')
    elif k == 'mute':
        if muted['vol'] is None:
            muted['vol'] = st['volume']; set_volume(0); toast('Muted')
        else:
            set_volume(muted['vol']); muted['vol'] = None; toast(f'Volume {st["volume"]}')
    elif k in ('ok', 'green'):
        s = cmd({'act': ['toggle']})
        toast('Playing' if s['state'] == 'playing' else 'Paused')
    elif k == 'right':
        cmd({'act': ['nextverse']}); toast('Next verse')
    elif k == 'left':
        cmd({'act': ['prevverse']}); toast('Previous verse')
    elif k == 'down':
        s = cmd({'act': ['next']}); toast(f'{s["surah"]}. {s["name"]}')
    elif k == 'up':
        s = cmd({'act': ['prev']}); toast(f'{s["surah"]}. {s["name"]}')
    elif k in ('back', 'home'):
        quran_hide(True)
        toast('Quran keeps playing' if st['state'] == 'playing' else 'Main page')
    elif k == 'menu':
        s = cmd({'act': ['repeat'], 'v': ['next' if st['repeat'] == 'surah' else 'surah']})
        toast('Repeat this surah' if s['repeat'] == 'surah' else 'Then play the next surah')

def remote_reader():
    # A blocking read() on hi_ir fails at once with ETIME, so wait with select() on a non-blocking fd.
    while True:
        try:
            fd = os.open('/dev/hi_ir', os.O_RDONLY | os.O_NONBLOCK)
        except OSError:
            time.sleep(30); continue
        while True:
            select.select([fd], [], [], 5)
            try:
                data = os.read(fd, 56 * 8)
            except OSError:
                time.sleep(0.05)             # EAGAIN / ETIME: nothing pressed
                continue
            for i in range(0, len(data) - 48, 56):
                b = data[i:i + 56]
                if b[48] == 2:
                    continue
                k = KEYS.get(b[8:12].hex())
                if b[48] == 0:
                    try:                     # last remote codes, for setting up buttons
                        with open('/run/ir-keys.log', 'a') as f:
                            f.write('%s %s %s\n' % (time.strftime('%H:%M:%S'), b[8:12].hex(), k or '?'))
                    except OSError:
                        pass
                if k and key_gate(k, b[48] == 1):
                    try:
                        remote_key(k, b[48] == 1)
                    except Exception as e:
                        print('remote error:', e, flush=True)

class H(BaseHTTPRequestHandler):
    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query, keep_blank_values=(u.path == "/video"))   # video: ls= is the top folder
        if u.path == '/cmd':
            body = cmd(q)
        elif u.path == '/video':
            body = video_api(q)
        elif u.path == '/airplay':
            body = airplay_api(q)
        elif u.path == '/chapters':
            body = {k: v['name'] + ' - ' + v['meaning'] for k, v in CH.items()}
        else:
            body = status()
        b = json.dumps(body).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def log_message(self, *a):
        pass

if __name__ == '__main__':
    set_volume(st['volume'])
    video_publish()                       # no video after a restart
    airplay_publish()
    menu_publish()
    threading.Thread(target=publisher, daemon=True).start()
    threading.Thread(target=remote_reader, daemon=True).start()
    ThreadingHTTPServer(('127.0.0.1', 8790), H).serve_forever()

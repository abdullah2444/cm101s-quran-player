"""Prepare Quran data for the CM101s-2 TV screen.
- pages/NNN.png : ink mask (L) of each Madani page, scaled to PH tall
- quran.json    : chapters (names, verse counts), verse->page, per-page verse boxes, verse timings (ms)
"""
import json, os, sqlite3, sys, urllib.request
from PIL import Image, ImageChops

SRC = r'Z:\CM101s-2\quran'
OUT = os.path.join(SRC, 'box')
PH = 940
PW = round(PH * 1024 / 1656)
S = PH / 1656

os.makedirs(os.path.join(OUT, 'pages'), exist_ok=True)

def get(url):
    for _ in range(4):
        try:
            return json.load(urllib.request.urlopen(urllib.request.Request(url, headers={'User-Agent': 'curl/8.0'}), timeout=30))
        except Exception as e:
            err = e
    raise err

# pages
if 'nopages' not in sys.argv:
    for p in range(1, 605):
        im = Image.open(os.path.join(SRC, 'width_1024', f'page{p:03d}.png')).convert('RGBA')
        r, g, b, a = im.split()
        ink = ImageChops.multiply(a, ImageChops.invert(im.convert('L')))
        ink = ink.resize((PW, PH), Image.LANCZOS)
        ink.save(os.path.join(OUT, 'pages', f'{p:03d}.png'), optimize=True)
    print('pages done', PW, PH)

# boxes: merge glyphs of each verse per line
db = sqlite3.connect(os.path.join(SRC, 'ayahinfo_1024.db'))
boxes, vpage = {}, {}
for page, line, s, a, x0, x1, y0, y1 in db.execute(
        'select page_number,line_number,sura_number,ayah_number,min_x,max_x,min_y,max_y from glyphs'):
    k = f'{s}:{a}'
    vpage.setdefault(k, page)
    vpage[k] = min(vpage[k], page)
    d = boxes.setdefault(str(page), {}).setdefault(k, {})
    b = d.get(line)
    d[line] = [min(b[0], x0), min(b[1], y0), max(b[2], x1), max(b[3], y1)] if b else [x0, y0, x1, y1]
pboxes = {pg: {k: [[round(c * S) for c in r] for _, r in sorted(v.items())] for k, v in d.items()}
          for pg, d in boxes.items()}

chapters = get('https://api.quran.com/api/v4/chapters?language=en')['chapters']
chap = {c['id']: {'name': c['name_simple'], 'meaning': c['translated_name']['name'], 'verses': c['verses_count'],
                  'place': c['revelation_place']} for c in chapters}

timings = {}
for c in range(1, 115):
    af = get(f'https://api.quran.com/api/v4/chapter_recitations/7/{c}?segments=true')['audio_file']
    timings[c] = {'bytes': int(af['file_size']),
                  'v': [[int(t['verse_key'].split(':')[1]), t['timestamp_from'], t['timestamp_to']] for t in af['timestamps']]}
    print(c, end=' ', flush=True)
print()

json.dump({'page_size': [PW, PH], 'chapters': chap, 'verse_page': vpage, 'boxes': pboxes, 'timings': timings},
          open(os.path.join(OUT, 'quran.json'), 'w'), separators=(',', ':'))
print('json', os.path.getsize(os.path.join(OUT, 'quran.json')))

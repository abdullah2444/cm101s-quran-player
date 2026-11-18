import json, os, sqlite3
from PIL import Image, ImageChops
SRC = os.environ.get('QURAN_SRC', './quran-raw')
OUT = os.path.join(SRC, 'box1260')
os.makedirs(os.path.join(OUT, 'pages'), exist_ok=True)
for p in range(1, 605):
    im = Image.open(os.path.join(SRC, 'width_1260', f'page{p:03d}.png')).convert('RGBA')
    ink = ImageChops.multiply(im.split()[3], ImageChops.invert(im.convert('L')))
    ink.save(os.path.join(OUT, 'pages', f'{p:03d}.png'), optimize=True)
db = sqlite3.connect(os.path.join(SRC, 'ayahinfo_1260.db'))
boxes, vpage = {}, {}
for page, line, s, a, x0, x1, y0, y1 in db.execute(
        'select page_number,line_number,sura_number,ayah_number,min_x,max_x,min_y,max_y from glyphs'):
    k = f'{s}:{a}'
    vpage[k] = min(vpage.get(k, page), page)
    d = boxes.setdefault(str(page), {}).setdefault(k, {})
    b = d.get(line)
    d[line] = [min(b[0], x0), min(b[1], y0), max(b[2], x1), max(b[3], y1)] if b else [x0, y0, x1, y1]
old = json.load(open(os.path.join(SRC, 'box', 'quran.json')))
old['page_size'] = [1260, 2038]
old['verse_page'] = vpage
old['boxes'] = {pg: {k: [r for _, r in sorted(v.items())] for k, v in d.items()} for pg, d in boxes.items()}
json.dump(old, open(os.path.join(OUT, 'quran.json'), 'w'), separators=(',', ':'))
print('done', os.path.getsize(os.path.join(OUT, 'quran.json')))

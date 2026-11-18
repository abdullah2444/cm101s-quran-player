<?php
require_once 'require_login.php';

function player($path) {
    $ctx = stream_context_create(['http' => ['timeout' => 8, 'proxy' => '']]);
    $r = @file_get_contents('http://127.0.0.1:8790' . $path, false, $ctx);
    return $r === false ? null : $r;
}

if (isset($_GET['api'])) {
    header('Content-Type: application/json');
    $api = $_GET['api'];
    if ($api === 'ls') {
        echo player('/video?' . http_build_query(['ls' => (string)($_GET['dir'] ?? '')])) ?? '{"offline":true}';
        exit;
    }
    if ($api === 'set') {
        if (!isset($_GET['csrf']) || !hash_equals($_SESSION['csrf_token'], $_GET['csrf'])) {
            http_response_code(403); echo '{"error":"session"}'; exit;
        }
        $q = [];
        if (isset($_GET['play'])) $q['play'] = (string)$_GET['play'];
        if (isset($_GET['start']) && ctype_digit($_GET['start'])) $q['start'] = $_GET['start'];
        if (isset($_GET['act']) && in_array($_GET['act'], ['toggle', 'stop', 'next', 'prev', 'seek', 'resume'], true)) $q['act'] = $_GET['act'];
        if (isset($_GET['t']) && is_numeric($_GET['t'])) $q['t'] = (string)max(0, (float)$_GET['t']);
        echo player('/video?' . http_build_query($q)) ?? '{"offline":true}';
        exit;
    }
    echo player('/video') ?? '{"offline":true}';
    exit;
}
?>
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="theme-color" content="#0f766e">
<link rel="shortcut icon" href="/favicon.ico">
<title>HiNAS - Videos</title>
<style>
:root {
  --bg:#f2f4f3; --card:#ffffff; --soft:#e8ecea; --text:#16201d; --muted:#66736f;
  --accent:#0f766e; --accent-soft:#d5ece9; --on-accent:#ffffff; --warn:#b45309;
}
@media (prefers-color-scheme: dark) {
  :root { --bg:#0e1312; --card:#171e1c; --soft:#222b29; --text:#e7eeec; --muted:#8c9a96;
          --accent:#2bb3a5; --accent-soft:#1d3532; --on-accent:#06201d; --warn:#f0a43a; }
}
* { box-sizing:border-box; -webkit-tap-highlight-color:transparent; }
body { margin:0; background:var(--bg); color:var(--text);
       font:15px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif; }
.wrap { max-width:560px; margin:0 auto; padding:20px 16px 40px; }
header { display:flex; align-items:center; justify-content:space-between; margin-bottom:14px; }
h1 { font-size:20px; font-weight:700; margin:0; letter-spacing:-.01em; }
a.back { color:var(--muted); text-decoration:none; font-size:14px; padding:6px 0; }
.card { background:var(--card); border-radius:16px; padding:20px; margin-bottom:12px; }
.label { font-size:12px; font-weight:600; text-transform:uppercase; letter-spacing:.07em; color:var(--muted); margin-bottom:10px; }
.err { background:#fde8e8; color:#9b1c1c; border-radius:12px; padding:12px 14px; margin-bottom:12px; display:none; font-size:14px; }

/* now playing */
.title { font-size:19px; font-weight:700; overflow-wrap:anywhere; }
.meta { color:var(--muted); font-size:14px; overflow-wrap:anywhere; }
.prog { margin:16px 0 6px; height:8px; background:var(--soft); border-radius:4px; cursor:pointer; position:relative; touch-action:none; }
.prog i { position:absolute; left:0; top:0; bottom:0; background:var(--accent); border-radius:4px; }
.times { display:flex; justify-content:space-between; color:var(--muted); font-size:13px; font-variant-numeric:tabular-nums; }
.ctl { display:flex; justify-content:center; align-items:center; gap:10px; margin-top:12px; }
.ctl button { border:0; background:var(--soft); color:var(--text); width:48px; height:48px; border-radius:50%;
              display:flex; align-items:center; justify-content:center; cursor:pointer; font:inherit; font-size:12px; font-weight:700; }
.ctl button.main { width:64px; height:64px; background:var(--accent); color:var(--on-accent); }
.ctl svg { width:22px; height:22px; fill:currentColor; }
.ctl button.main svg { width:28px; height:28px; }
.ctl button:active { transform:scale(.94); }
.wide { display:block; width:100%; margin-top:14px; border:0; border-radius:12px; padding:12px; background:var(--accent); color:var(--on-accent);
        font:inherit; font-weight:600; cursor:pointer; }

/* browser */
.crumbs { display:flex; flex-wrap:wrap; gap:4px; align-items:center; margin-bottom:8px; font-size:14px; }
.crumbs a { color:var(--accent); text-decoration:none; cursor:pointer; }
.crumbs span { color:var(--muted); }
.item { display:flex; align-items:center; gap:12px; padding:11px 4px; cursor:pointer; border-radius:10px; }
.item + .item { border-top:1px solid var(--soft); }
.item:active { background:var(--soft); }
.item svg { flex:none; width:22px; height:22px; fill:var(--muted); }
.item .n { flex:1; min-width:0; overflow-wrap:anywhere; }
.item .s { flex:none; font-size:12px; color:var(--warn); font-variant-numeric:tabular-nums; }
.item.now .n { color:var(--accent); font-weight:600; }
.empty { color:var(--muted); font-size:14px; padding:8px 4px; }
.hint { color:var(--muted); font-size:13px; margin-top:4px; }
</style>
</head>
<body>
<div class="wrap">
  <header><h1>Videos</h1><a class="back" href="/home.php">&larr; Panel</a></header>
  <div class="err" id="err"></div>

  <div class="card" id="nowcard">
    <div class="label">On the TV</div>
    <div class="title" id="title">Nothing playing</div>
    <div class="meta" id="folder"></div>
    <div id="playing" style="display:none">
      <div class="prog" id="prog"><i id="bar"></i></div>
      <div class="times"><span id="pos">0:00</span><span id="dur">0:00</span></div>
      <div class="ctl">
        <button id="prev" aria-label="Previous episode"><svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zM20 5v14L9 12z"/></svg></button>
        <button id="back10" aria-label="Back 10 seconds">-10</button>
        <button id="toggle" class="main" aria-label="Play or pause"><svg viewBox="0 0 24 24" id="ticon"></svg></button>
        <button id="fwd10" aria-label="Forward 10 seconds">+10</button>
        <button id="next" aria-label="Next episode"><svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM4 5v14l11-7z"/></svg></button>
      </div>
      <button class="wide" id="stop" style="background:var(--soft);color:var(--text)">Stop</button>
    </div>
    <button class="wide" id="resume" style="display:none">Continue</button>
  </div>

  <div class="card">
    <div class="label">Media library</div>
    <div class="crumbs" id="crumbs"></div>
    <div id="list"><div class="empty">Loading…</div></div>
    <div class="hint">Plays H.264 and H.265 videos (MP4, MKV, MOV, TS). Remote: OK pause · ◀ ▶ 10 s · ▲ ▼ episode · Back stop · Set = carry on with the last video.</div>
  </div>
</div>
<script>
const CSRF = <?= json_encode($_SESSION['csrf_token']) ?>;
const el = id => document.getElementById(id);
const esc = s => String(s).replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
const tm = ms => { let s = Math.max(0, Math.floor(ms / 1000)), h = Math.floor(s / 3600), m = Math.floor(s % 3600 / 60); s %= 60;
                   return (h ? h + ':' + String(m).padStart(2, '0') : m) + ':' + String(s).padStart(2, '0'); };
const ICON = { folder: '<svg viewBox="0 0 24 24"><path d="M3 6a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>',
               file: '<svg viewBox="0 0 24 24"><path d="M4 5h16v14H4zM10 9v6l5-3z"/></svg>',
               play: '<path d="M7 4v16l13-8z"/>', pause: '<path d="M6 4h4v16H6zM14 4h4v16h-4z"/>' };
let cur = {}, dir = '', listing = null;

function showErr(t) { el('err').textContent = t || ''; el('err').style.display = t ? 'block' : 'none'; }

function show(v) {
  if (!v) return;
  cur = v;
  el('title').textContent = v.on ? v.title : (v.title ? 'Last: ' + v.title : 'Nothing playing');
  el('folder').textContent = v.folder || '';
  el('playing').style.display = v.on ? 'block' : 'none';
  el('resume').style.display = !v.on && v.rel ? 'block' : 'none';
  el('resume').textContent = v.pos > 0 ? 'Continue from ' + tm(v.pos) : 'Play it again';
  if (v.on) {
    el('pos').textContent = tm(v.pos); el('dur').textContent = tm(v.dur);
    el('bar').style.width = (v.dur ? Math.min(100, v.pos / v.dur * 100) : 0) + '%';
    el('ticon').innerHTML = v.paused ? ICON.play : ICON.pause;
  }
  for (const it of document.querySelectorAll('.item[data-f]'))
    it.classList.toggle('now', v.on && it.dataset.f === v.rel);
}

async function get(params) {
  try {
    const r = await fetch('videos.php?' + new URLSearchParams(params));
    if (r.status === 403) { location.reload(); return null; }
    const j = await r.json();
    if (j.offline) { showErr('The Quran player is off. It runs the video player too, so turn it on in Services.'); return null; }
    showErr(j.error || '');
    return j;
  } catch (e) { return null; }
}
async function set(extra) { const j = await get(Object.assign({api: 'set', csrf: CSRF}, extra)); if (j) show(j.video); }

async function browse(d) {
  const j = await get({api: 'ls', dir: d});
  if (j && j.error && d) return browse('');          // remembered folder is gone: start at the top
  if (!j || j.error) { el('list').innerHTML = '<div class="empty">' + esc(j && j.error ? 'Cannot open this folder: ' + j.error : 'Cannot open media folder.') + '</div>'; return; }
  dir = j.dir; listing = j;
  let parts = dir ? dir.split('/') : [], h = '<a data-d="">Media</a>';
  parts.forEach((p, i) => { h += '<span>/</span><a data-d="' + esc(parts.slice(0, i + 1).join('/')) + '">' + esc(p) + '</a>'; });
  el('crumbs').innerHTML = h;
  let L = '';
  for (const f of j.dirs) L += '<div class="item" data-d="' + esc((dir ? dir + '/' : '') + f) + '">' + ICON.folder + '<span class="n">' + esc(f) + '</span></div>';
  for (const f of j.files) {
    const rel = (dir ? dir + '/' : '') + f, s = j.saved[f];
    L += '<div class="item" data-f="' + esc(rel) + '">' + ICON.file + '<span class="n">' + esc(f.replace(/\.[^.]+$/, '')) + '</span>' +
         (s ? '<span class="s">' + tm(s) + '</span>' : '') + '</div>';
  }
  el('list').innerHTML = L || '<div class="empty">No videos here.</div>';
  show(j.video);
  try { localStorage.setItem('videos.dir', dir); } catch (e) {}
}

el('crumbs').addEventListener('click', e => { const a = e.target.closest('a'); if (a) browse(a.dataset.d); });
el('list').addEventListener('click', e => {
  const it = e.target.closest('.item'); if (!it) return;
  if (it.dataset.d !== undefined) browse(it.dataset.d);
  else set({play: it.dataset.f});
});
el('toggle').onclick = () => set({act: 'toggle'});
el('stop').onclick = () => set({act: 'stop'}).then(() => browse(dir));
el('next').onclick = () => set({act: 'next'});
el('prev').onclick = () => set({act: 'prev'});
el('back10').onclick = () => set({act: 'seek', t: Math.max(0, (cur.pos - 10000) / 1000).toFixed(1)});
el('fwd10').onclick = () => set({act: 'seek', t: ((cur.pos + 10000) / 1000).toFixed(1)});
el('resume').onclick = () => set({act: 'resume'});
el('prog').addEventListener('click', e => {
  if (!cur.dur) return;
  const r = el('prog').getBoundingClientRect(), f = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
  set({act: 'seek', t: (f * cur.dur / 1000).toFixed(1)});
});

let start = '';
try { start = localStorage.getItem('videos.dir') || ''; } catch (e) {}
browse(start);
async function poll() { const j = await get({api: 'state'}); if (j) show(j.video); setTimeout(poll, cur.on ? 1000 : 3000); }
setTimeout(poll, 1000);
</script>
</body>
</html>

<?php
require_once 'require_login.php';

function player($path) {
    $ctx = stream_context_create(['http' => ['timeout' => 3, 'proxy' => '']]);
    $r = @file_get_contents('http://127.0.0.1:8790' . $path, false, $ctx);
    return $r === false ? null : $r;
}

if (isset($_GET['api'])) {
    header('Content-Type: application/json');
    if ($_GET['api'] === 'state') {
        echo player('/') ?? '{"offline":true}';
    } elseif ($_GET['api'] === 'cmd') {
        if (!isset($_GET['csrf']) || !hash_equals($_SESSION['csrf_token'], $_GET['csrf'])) {
            http_response_code(403); echo '{"error":"session"}'; exit;
        }
        $acts = ['play', 'resume', 'pause', 'toggle', 'stop', 'next', 'prev', 'nextverse', 'prevverse', 'volume', 'repeat'];
        $act = $_GET['act'] ?? '';
        if (!in_array($act, $acts, true)) { http_response_code(400); echo '{"error":"act"}'; exit; }
        $q = ['act' => $act];
        foreach (['surah', 'verse', 'v'] as $k) {
            if (isset($_GET[$k]) && preg_match('/^[a-z0-9]{1,6}$/', $_GET[$k])) $q[$k] = $_GET[$k];
        }
        echo player('/cmd?' . http_build_query($q)) ?? '{"offline":true}';
    }
    exit;
}
// Surah names only (the player sends "Name - meaning"; the meaning is dropped here)
$names = [];
foreach (json_decode(player('/chapters') ?? '{}', true) ?: [] as $n => $label) {
    $names[(int)$n] = explode(' - ', $label)[0];
}
?>
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="theme-color" content="#0f766e">
<link rel="shortcut icon" href="/favicon.ico">
<title>HiNAS - Quran</title>
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
.np { display:flex; align-items:center; gap:14px; }
.num { flex:none; width:52px; height:52px; border-radius:14px; background:var(--accent-soft); color:var(--accent);
       display:flex; align-items:center; justify-content:center; font-size:18px; font-weight:700; }
.np-text { min-width:0; flex:1; }
.name { font-size:22px; font-weight:700; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.meta { color:var(--muted); font-size:14px; }
.pill { display:inline-block; font-size:11px; font-weight:700; text-transform:uppercase; letter-spacing:.06em;
        padding:3px 8px; border-radius:99px; background:var(--soft); color:var(--muted); vertical-align:middle; }
.pill.playing { background:var(--accent-soft); color:var(--accent); }
.pill.paused { color:var(--warn); }
.track { height:6px; background:var(--soft); border-radius:3px; margin:18px 0 6px; overflow:hidden; }
.track i { display:block; height:100%; width:0; background:var(--accent); border-radius:3px; transition:width .4s linear; }
.times { display:flex; justify-content:space-between; color:var(--muted); font-size:12px; font-variant-numeric:tabular-nums; }

/* transport */
.ctrls { display:flex; align-items:center; justify-content:center; gap:10px; margin-top:16px; }
.ib { border:0; background:var(--soft); color:var(--text); width:48px; height:48px; border-radius:50%;
      display:flex; align-items:center; justify-content:center; cursor:pointer; padding:0; }
.ib svg { width:22px; height:22px; fill:currentColor; }
.ib.big { width:68px; height:68px; background:var(--accent); color:var(--on-accent); }
.ib.big svg { width:30px; height:30px; }
.ib:active { transform:scale(.94); }
.ib small { font-size:10px; font-weight:700; margin-left:-2px; }
.sub-ctrls { display:flex; justify-content:center; gap:8px; margin-top:12px; }
.chip { border:0; background:transparent; color:var(--muted); font:inherit; font-size:13px; font-weight:600;
        padding:6px 12px; border-radius:99px; cursor:pointer; }
.chip:active { background:var(--soft); }

/* go to */
.grid { display:grid; grid-template-columns:1fr 110px; gap:10px; }
.field { display:flex; flex-direction:column; gap:6px; min-width:0; }
.field span { font-size:13px; color:var(--muted); }
select { appearance:none; -webkit-appearance:none; width:100%; border:0; background:var(--soft); color:var(--text);
         font:inherit; font-size:16px; padding:12px 34px 12px 14px; border-radius:12px; cursor:pointer;
         background-image:linear-gradient(45deg,transparent 50%,var(--muted) 50%),linear-gradient(135deg,var(--muted) 50%,transparent 50%);
         background-position:calc(100% - 18px) 52%,calc(100% - 13px) 52%; background-size:5px 5px; background-repeat:no-repeat; }
.btn { width:100%; border:0; background:var(--accent); color:var(--on-accent); font:inherit; font-size:16px; font-weight:700;
       padding:13px; border-radius:12px; margin-top:12px; cursor:pointer; }
.btn:active { opacity:.85; }

/* settings */
.row { display:flex; align-items:center; gap:12px; }
.row + .row { margin-top:18px; }
.row .k { width:92px; flex:none; font-size:14px; color:var(--muted); }
input[type=range] { -webkit-appearance:none; appearance:none; flex:1; height:6px; border-radius:3px; background:var(--soft); outline:none; }
input[type=range]::-webkit-slider-thumb { -webkit-appearance:none; width:22px; height:22px; border-radius:50%; background:var(--accent); border:0; cursor:pointer; }
input[type=range]::-moz-range-thumb { width:22px; height:22px; border-radius:50%; background:var(--accent); border:0; cursor:pointer; }
.vol { width:28px; text-align:right; font-variant-numeric:tabular-nums; font-weight:600; }
.seg { flex:1; display:flex; background:var(--soft); border-radius:12px; padding:4px; }
.seg button { flex:1; border:0; background:transparent; color:var(--muted); font:inherit; font-size:14px; font-weight:600;
              padding:9px 6px; border-radius:9px; cursor:pointer; }
.seg button.on { background:var(--card); color:var(--text); }
</style>
</head>
<body>
<div class="wrap">
  <header><h1>Quran</h1><a class="back" href="/home.php">&larr; Panel</a></header>
  <div class="err" id="err">The Quran player is off. Turn it on in Services.</div>

  <div class="card">
    <div class="np">
      <div class="num" id="num">-</div>
      <div class="np-text">
        <div class="name" id="name">-</div>
        <div class="meta"><span id="verseinfo">-</span> &nbsp;<span class="pill" id="state">stopped</span></div>
      </div>
    </div>
    <div class="track"><i id="bar"></i></div>
    <div class="times"><span id="pos">0:00</span><span id="len">0:00</span></div>
    <div class="ctrls">
      <button class="ib" onclick="cmd('prev')" title="Previous surah" aria-label="Previous surah">
        <svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zM20 5v14L9.5 12z"/></svg></button>
      <button class="ib" onclick="cmd('prevverse')" title="Previous verse" aria-label="Previous verse">
        <svg viewBox="0 0 24 24"><path d="M12 5v14L3 12zM21 5v14l-9-7z"/></svg></button>
      <button class="ib big" id="pp" onclick="cmd('toggle')" aria-label="Play or pause"></button>
      <button class="ib" onclick="cmd('nextverse')" title="Next verse" aria-label="Next verse">
        <svg viewBox="0 0 24 24"><path d="M3 5v14l9-7zM12 5v14l9-7z"/></svg></button>
      <button class="ib" onclick="cmd('next')" title="Next surah" aria-label="Next surah">
        <svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM4 5v14l10.5-7z"/></svg></button>
    </div>
    <div class="sub-ctrls">
      <button class="chip" onclick="cmd('stop')">Stop</button>
    </div>
  </div>

  <div class="card">
    <div class="label">Go to</div>
    <div class="grid">
      <label class="field"><span>Surah</span>
        <select id="surah">
          <?php foreach ($names as $n => $name): ?><option value="<?= $n ?>"><?= $n ?>. <?= htmlspecialchars($name, ENT_QUOTES, 'UTF-8') ?></option><?php endforeach; ?>
        </select></label>
      <label class="field"><span>Verse</span><select id="verse"></select></label>
    </div>
    <button class="btn" onclick="cmd('play', {surah: el('surah').value, verse: el('verse').value})">Play from here</button>
  </div>

  <div class="card">
    <div class="row"><span class="k">Volume</span>
      <input type="range" id="vol" min="0" max="99"><span class="vol" id="volv">-</span></div>
    <div class="row"><span class="k">When a surah ends</span>
      <div class="seg" id="repeat">
        <button data-v="next" onclick="cmd('repeat', {v: 'next'})">Next surah</button>
        <button data-v="surah" onclick="cmd('repeat', {v: 'surah'})">Repeat</button>
      </div></div>
  </div>
</div>
<script>
const CSRF = <?= json_encode($_SESSION['csrf_token']) ?>;
const VERSES = [0,7,286,200,176,120,165,206,75,129,109,123,111,43,52,99,128,111,110,98,135,112,78,118,64,77,227,93,88,69,60,34,30,73,54,45,83,182,88,75,85,54,53,89,59,37,35,38,29,18,45,60,49,62,55,78,96,29,22,24,13,14,11,11,18,12,12,30,52,52,44,28,28,20,56,40,31,50,40,46,42,29,19,36,25,22,17,19,26,30,20,15,21,11,8,8,19,5,8,8,11,11,8,3,9,5,4,7,3,6,3,5,4,5,6];
const ICON_PLAY = '<svg viewBox="0 0 24 24"><path d="M8 5v14l11-7z"/></svg>';
const ICON_PAUSE = '<svg viewBox="0 0 24 24"><path d="M6 5h4v14H6zM14 5h4v14h-4z"/></svg>';
const el = id => document.getElementById(id);
let volBusy = false, first = true, lastPP = '';

function fillVerses(n, pick) {
  const s = el('verse'), opts = [];
  for (let i = 1; i <= n; i++) opts.push(`<option value="${i}">${i}</option>`);
  s.innerHTML = opts.join('');
  s.value = Math.min(pick || 1, n);
}
el('surah').addEventListener('change', e => fillVerses(VERSES[e.target.value], 1));

function fmt(ms) {
  const s = Math.floor(ms / 1000), h = Math.floor(s / 3600), m = Math.floor(s % 3600 / 60), x = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${x}` : `${m}:${x}`;
}
function show(s) {
  el('err').style.display = s.offline ? 'block' : 'none';
  if (s.offline || s.error) return;
  el('num').textContent = s.surah;
  el('name').textContent = s.name;
  el('verseinfo').textContent = `Verse ${s.verse} of ${s.verses}`;
  el('state').textContent = s.state; el('state').className = 'pill ' + s.state;
  el('bar').style.width = (s.length_ms ? Math.min(100, s.pos_ms / s.length_ms * 100) : 0) + '%';
  el('pos').textContent = fmt(s.pos_ms); el('len').textContent = fmt(s.length_ms);
  const pp = s.state === 'playing' ? ICON_PAUSE : ICON_PLAY;
  if (pp !== lastPP) { el('pp').innerHTML = pp; lastPP = pp; }
  if (!volBusy) { el('vol').value = s.volume; el('volv').textContent = s.volume; }
  for (const b of el('repeat').children) b.classList.toggle('on', b.dataset.v === s.repeat);
  if (first) { el('surah').value = s.surah; fillVerses(s.verses, s.verse); first = false; }
}
async function poll() { try { show(await (await fetch('quran.php?api=state')).json()); } catch (e) {} }
async function cmd(act, extra) {
  const p = new URLSearchParams(Object.assign({api: 'cmd', act, csrf: CSRF}, extra || {}));
  try { const r = await fetch('quran.php?' + p); if (r.status === 403) return location.reload(); show(await r.json()); } catch (e) {}
}
el('vol').addEventListener('input', e => { volBusy = true; el('volv').textContent = e.target.value; });
el('vol').addEventListener('change', e => { cmd('volume', {v: e.target.value}); setTimeout(() => volBusy = false, 1500); });
el('pp').innerHTML = ICON_PLAY;
fillVerses(VERSES[1], 1);
poll(); setInterval(poll, 1500);
</script>
</body>
</html>

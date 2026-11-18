<?php
require_once 'require_login.php';
if (isset($_GET['api'])) {
    header('Content-Type: application/json');
    header('Cache-Control: no-store');
    $path = '/airplay';
    if ($_SERVER['REQUEST_METHOD'] === 'POST') {
        if (!isset($_POST['csrf']) || !hash_equals($_SESSION['csrf_token'], (string)$_POST['csrf'])) {
            http_response_code(403); echo '{"error":"Session expired. Reload the page."}'; exit;
        }
        $action = (string)($_POST['action'] ?? '');
        if (!in_array($action, ['start', 'stop'], true)) {
            http_response_code(400); echo '{"error":"Unknown action."}'; exit;
        }
        $path .= '?act=' . rawurlencode($action);
    } elseif ($_SERVER['REQUEST_METHOD'] !== 'GET') {
        http_response_code(405); exit;
    }
    // Release the PHP session lock while the player changes modes.
    session_write_close();
    $ctx = stream_context_create(['http' => ['timeout' => 25, 'proxy' => '']]);
    $result = @file_get_contents('http://127.0.0.1:8790' . $path, false, $ctx);
    echo $result === false ? '{"error":"Player is unavailable."}' : $result;
    exit;
}
?>
<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="theme-color" content="#0f766e"><link rel="shortcut icon" href="/favicon.ico">
<title>HiNAS - AirPlay</title>
<style>
:root{color-scheme:light dark;--bg:#f2f4f3;--card:#fff;--text:#16201d;--muted:#66736f;--accent:#0f766e;--soft:#e8ecea}
@media(prefers-color-scheme:dark){:root{--bg:#0e1312;--card:#171e1c;--text:#e7eeec;--muted:#9aa9a2;--accent:#2bb3a5;--soft:#222b29}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:560px;margin:auto;padding:20px 16px 40px}header{display:flex;align-items:center;justify-content:space-between;margin-bottom:14px}
h1{font-size:20px;margin:0}h2{font-size:20px;margin:0 0 6px}a{color:var(--muted);text-decoration:none}
.card{background:var(--card);border-radius:16px;padding:22px;margin-bottom:12px}.label{font-size:12px;font-weight:700;letter-spacing:.06em;text-transform:uppercase;color:var(--muted)}
.value{font-size:21px;font-weight:700;margin:3px 0 16px;overflow-wrap:anywhere}.muted{color:var(--muted)}
button{width:100%;border:0;border-radius:12px;padding:14px;background:var(--accent);color:white;font:inherit;font-weight:700;cursor:pointer}button:disabled{opacity:.55;cursor:wait}
.error{color:#b45309;white-space:pre-wrap}.steps{padding-left:20px;margin-bottom:0}.steps li{margin:8px 0}
</style></head><body><main>
<header><h1>AirPlay</h1><a href="/home.php">← Home</a></header>
<section class="card"><div class="label">Status</div><h2 id="status">Checking…</h2>
<p class="muted" id="detail">Connect your iPhone to the same network as the box.</p>
<p class="error" id="error" role="alert"></p>
<button id="toggle" disabled>Loading…</button></section>
<section class="card"><div class="label">Receiver</div><div class="value" id="name">CM101s AirPlay</div>
<div class="label">Password</div><div class="value" id="password">—</div>
<p class="muted">Your iPhone may remember the password after the first connection.</p></section>
<section class="card"><div class="label">Connect your iPhone</div><ol class="steps">
<li>Start AirPlay here, then open Control Center on your iPhone.</li>
<li>Tap Screen Mirroring and choose <b>CM101s AirPlay</b>.</li>
<li>Enter the password above if prompted.</li></ol>
<p class="muted">Starting AirPlay stops current video and Quran playback. Press Back on the box remote to stop AirPlay.</p></section>
</main><script>
const csrf = <?= json_encode($_SESSION['csrf_token']) ?>;
let state = null, busy = false;
const el = id => document.getElementById(id);
function render(data) {
  state = data; el('error').textContent = data.error || '';
  el('name').textContent = data.name || 'CM101s AirPlay';
  el('password').textContent = data.password || '—';
  el('status').textContent = data.on ? (data.streaming ? 'Mirroring' : 'Ready to connect') : 'Off';
  el('detail').textContent = data.on ? 'Use Screen Mirroring on your iPhone.' : 'Start the receiver to connect your iPhone.';
  el('toggle').textContent = data.on ? 'Stop AirPlay' : 'Start AirPlay';
  el('toggle').disabled = busy || !data.name;
}
async function refresh() {
  if (busy) return;
  try { const r = await fetch('?api=status', {cache:'no-store'}); render(await r.json()); }
  catch (_) { render({error:'Could not reach the player.'}); }
}
el('toggle').onclick = async () => {
  if (!state || busy) return;
  busy = true; el('toggle').disabled = true;
  try {
    const body = new URLSearchParams({csrf, action:state.on ? 'stop' : 'start'});
    const r = await fetch('?api=control', {method:'POST', body, cache:'no-store'});
    render(await r.json());
  } catch (_) { el('error').textContent = 'Could not reach the player.'; }
  finally { busy = false; if (state) el('toggle').disabled = !state.name; }
};
refresh(); setInterval(refresh, 2000);
</script></body></html>

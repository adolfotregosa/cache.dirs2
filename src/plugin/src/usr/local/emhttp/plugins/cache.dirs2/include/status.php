<?php
/* Cache Directories 2.0 — renders the live status table. Reads only RAM-backed files. */
$file = '/var/local/cache.dirs2/status.json';
$pidfile = '/var/run/cachedirs2.pid';

function h($v) { return htmlspecialchars((string)$v, ENT_QUOTES); }
function ago($t) {
  if (!$t) return '—';
  $d = time() - $t;
  if ($d < 60) return $d.'s ago';
  if ($d < 3600) return intdiv($d, 60).'m ago';
  if ($d < 86400) return intdiv($d, 3600).'h ago';
  return intdiv($d, 86400).'d ago';
}
function bytes($b) {
  $u = ['B','KiB','MiB','GiB','TiB']; $i = 0;
  while ($b >= 1024 && $i < 4) { $b /= 1024; $i++; }
  return sprintf('%.1f %s', $b, $u[$i]);
}

$pid = is_file($pidfile) ? trim(file_get_contents($pidfile)) : '';
$running = $pid !== '' && ctype_digit($pid) && is_dir("/proc/$pid");

if (!$running) {
  echo '<p class="muted">Not running. Set “Enable folder caching” to Yes and apply; it runs while the array is started.</p>';
  exit;
}
$s = is_file($file) ? json_decode(@file_get_contents($file), true) : null;
if (!$s) {
  echo '<p class="muted">Starting… the first pass over each disk can take a minute.</p>';
  exit;
}

$cache = (int)$s['cache_bytes'];
$mem = max(1, (int)$s['mem_total']);
$pct = round($cache * 100 / $mem, 1);
$over = $pct > (int)$s['budget_pct'];
$total = 0;
foreach ($s['targets'] as $t) $total += (int)$t['entries'];

echo '<p>Running (pid '.h($s['pid']).'), refreshing every '.h($s['interval']).'s, last pass '.ago($s['last_cycle']).'. ';
echo number_format($total).' entries cached. Kernel metadata cache: <span'.($over ? ' class="woke"' : '').'>'.bytes($cache).' ('.$pct.'% of RAM)</span>.</p>';
if (!empty($s['message'])) echo '<p class="muted">'.h($s['message']).'</p>';
if (!$s['targets']) exit;

echo '<table><thead><tr><th>Disk</th><th>Device</th><th>FS</th><th>State</th><th class="num">Entries</th>';
echo '<th>Last refresh</th><th class="num">Took</th><th>Note</th></tr></thead><tbody>';
foreach ($s['targets'] as $t) {
  $state = $t['state'] === 'asleep' ? '<span class="asleep">asleep</span>'
         : ($t['state'] === 'awake' ? '<span class="awake">awake</span>' : '<span class="muted">unknown</span>');
  $woke = (int)$t['wakes'];
  $note = [];
  if ($t['skipped']) $note[] = h($t['skipped']);
  if (!empty($t['backoff_until']) && $t['backoff_until'] > time()) $note[] = 'back-off ends in '.round(($t['backoff_until'] - time()) / 60).'m';
  if (!$t['noatime']) $note[] = 'not mounted noatime';
  if ($woke > 0) $note[] = '<span class="woke">woken by refresh '.$woke.'× (last '.ago($t['last_wake']).')</span>';
  if ((int)$t['errors'] > 0) $note[] = h($t['errors']).' unreadable entries';
  echo '<tr>';
  echo '<td>'.h($t['name']).'</td>';
  echo '<td class="muted">'.h(implode(', ', $t['devices'])).'</td>';
  echo '<td class="muted">'.h($t['fs']).'</td>';
  echo '<td>'.$state.'</td>';
  echo '<td class="num">'.number_format((int)$t['entries']).'</td>';
  echo '<td>'.ago($t['last_refresh']).'</td>';
  echo '<td class="num">'.sprintf('%.1fs', $t['last_ms'] / 1000).'</td>';
  echo '<td class="muted">'.implode('; ', $note).'</td>';
  echo '</tr>';
}
echo '</tbody></table>';

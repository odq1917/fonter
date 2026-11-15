'use strict';
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);

const ROW = 74;
const DEFAULT_LINE = '가나다라 Aa Bb 123 あいう アイウ 漢字 永遠';
const SAMPLE = `沈むように溶けてゆくように
二人だけの空が広がる夜に

「さよなら」だけだった
その一言で全てが分かった
日が沈み出した空と君の姿
フェンス越しに重なっていた

初めて会った日から
僕の心の全てを奪った ♪`;
const TEXT_EXT = /\.(txt|lrc|srt|ass|ssa|vtt|smi|md|csv|json)$/i;

const S = {
  faces: [], counts: null, groups: [], view: [],
  tab: 'full', search: '', text: '',
  primary: [], secondary: [], primarySet: new Set(), allCps: [],
  gen: 0, missing: new Map(), sel: null, selFace: null, line: DEFAULT_LINE,
};

const nf = new Intl.NumberFormat('ko-KR');
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const hex = (cp) => 'U+' + cp.toString(16).toUpperCase().padStart(4, '0');

function toast(msg) {
  const t = $('toast');
  t.textContent = msg;
  t.classList.add('on');
  clearTimeout(toast.t);
  toast.t = setTimeout(() => t.classList.remove('on'), 2200);
}

// ------------------------------------------------------------ text analysis
const IGNORE = /[\p{Z}\p{C}]/u;
const PRIMARY = /[\p{L}\p{N}]/u;
const isVariationSelector = (cp) => (cp >= 0xFE00 && cp <= 0xFE0F) || (cp >= 0xE0100 && cp <= 0xE01EF) || (cp >= 0x180B && cp <= 0x180D);

function analyze(raw) {
  const text = raw.normalize('NFC');
  const seen = new Set();
  const primary = [], secondary = [];
  const k = { 한글: 0, 가나: 0, 한자: 0, 라틴: 0, 숫자: 0, 기호: 0 };
  let total = 0;
  for (const ch of text) {
    const cp = ch.codePointAt(0);
    if (IGNORE.test(ch) || isVariationSelector(cp)) continue;
    total++;
    if (seen.has(cp)) continue;
    seen.add(cp);
    if (PRIMARY.test(ch)) {
      primary.push(cp);
      if (/\p{Script=Hangul}/u.test(ch)) k.한글++;
      else if (/[\p{Script=Hiragana}\p{Script=Katakana}]/u.test(ch)) k.가나++;
      else if (/\p{Script=Han}/u.test(ch)) k.한자++;
      else if (/\p{Script=Latin}/u.test(ch)) k.라틴++;
      else if (/\p{N}/u.test(ch)) k.숫자++;
      else k.기호++;
    } else {
      secondary.push(cp);
      k.기호++;
    }
  }
  return { text, primary, secondary, total, kinds: k };
}

function renderStats(a) {
  if (!a.total) { $('stats').innerHTML = '<span class="stat">텍스트를 넣으면 실시간으로 검사합니다</span>'; return; }
  let h = `<span class="stat main">전체<b>${nf.format(a.total)}</b></span><span class="stat main">고유 글자<b>${nf.format(a.primary.length + a.secondary.length)}</b></span>`;
  for (const [name, n] of Object.entries(a.kinds)) if (n) h += `<span class="stat">${name}<b>${nf.format(n)}</b></span>`;
  $('stats').innerHTML = h;
}

// ------------------------------------------------------------------ library
async function loadLibrary(fresh = false) {
  $('progress').classList.add('on');
  $('libStatus').textContent = '폰트 스캔 중…';
  const lib = await invoke('load_library', { fresh });
  $('progress').classList.remove('on');
  $('progress').firstElementChild.style.width = '0';

  const collator = new Intl.Collator('ko');
  const order = [...new Set(lib.faces.map((f) => f.family))].sort(collator.compare);
  const rank = new Map(order.map((f, i) => [f, i]));
  for (const f of lib.faces) { f.ord = rank.get(f.family); f.lower = (f.family + ' ' + f.full + ' ' + f.ps).toLowerCase(); }
  S.faces = lib.faces;
  S.sel = null; S.selFace = null;
  $('drawer').hidden = true;

  const how = lib.parsed ? `${nf.format(lib.parsed)}개 파일 파싱` : '캐시 사용';
  $('libStatus').textContent = `폰트 ${nf.format(order.length)}종 · 스타일 ${nf.format(lib.faces.length)}개 · ${how} ${lib.ms.toFixed(0)}ms`;
  const chip = $('extraChip');
  chip.hidden = !lib.extra.length;
  chip.title = lib.extra.join('\n');
  chip.innerHTML = `추가한 경로 ${lib.extra.length}곳<button title="추가한 경로 모두 제거">✕</button>`;
  await runQuery();
}

async function addSources(paths) {
  if (!paths || !paths.length) return;
  const added = await invoke('add_sources', { paths });
  if (!added) return toast('추가할 폰트나 폴더가 없습니다');
  await loadLibrary();
  toast(`경로 ${added}곳을 검사 대상에 추가했습니다`);
}

// -------------------------------------------------------------------- query
const tier = (p, s) => (p ? 'partial' : s ? 'letters' : 'full');

async function runQuery() {
  const a = analyze($('text').value);
  const gen = ++S.gen;
  S.text = a.text;
  S.primary = a.primary; S.secondary = a.secondary;
  S.primarySet = new Set(a.primary);
  S.allCps = a.primary.concat(a.secondary);
  S.missing = new Map();
  S.line = (a.text.split('\n').map((l) => l.trim()).find((l) => l.length > 1) || DEFAULT_LINE).slice(0, 60);
  renderStats(a);
  try { localStorage.setItem('fonter.text', $('text').value.slice(0, 200000)); } catch {}

  const idle = !S.allCps.length;
  let counts, micros = 0;
  if (idle) counts = new Uint32Array(S.faces.length * 2);
  else {
    const r = await invoke('query', { primary: a.primary, secondary: a.secondary });
    if (gen !== S.gen) return;
    counts = r.counts; micros = r.micros;
  }
  S.counts = counts;

  // Group styles of one family that share the same result into a single row.
  const map = new Map();
  for (const f of S.faces) {
    const p = counts[f.id * 2], s = counts[f.id * 2 + 1];
    const key = f.family + '\u0001' + p + '\u0001' + s;
    let g = map.get(key);
    if (!g) map.set(key, (g = { family: f.family, ord: f.ord, p, s, tier: tier(p, s), faces: [], lower: '' }));
    g.faces.push(f);
    g.lower += f.lower + ' ';
  }
  const score = (f) => (f.italic ? 1000 : 0) + Math.abs(f.weight - 400);
  S.groups = [...map.values()];
  for (const g of S.groups) { g.faces.sort((x, y) => score(x) - score(y) || x.weight - y.weight); g.rep = g.faces[0]; }
  S.groups.sort((x, y) => x.p - y.p || x.s - y.s || x.ord - y.ord);

  const n = { full: 0, letters: 0, partial: 0 };
  for (const g of S.groups) n[g.tier]++;
  $('tabs').classList.toggle('idle', idle);
  for (const b of $('tabs').children) {
    const t = b.dataset.tab;
    b.querySelector('b').textContent = nf.format(t === 'all' ? S.groups.length : n[t]);
  }
  $('timing').textContent = idle ? '' : `스타일 ${nf.format(S.faces.length)}개 검사 ${micros < 1000 ? micros.toFixed(0) + 'µs' : (micros / 1000).toFixed(2) + 'ms'}`;
  const keep = S.selFace;
  applyFilter(true);
  if (keep) {
    const again = S.groups.find((g) => g.faces.includes(keep));
    if (again) openDrawer(again, keep); else closeDrawer();
  }
}

function applyFilter(resetScroll) {
  const idle = !S.allCps.length;
  const tab = idle ? 'all' : S.tab;
  const words = S.search.toLowerCase().split(/\s+/).filter(Boolean);
  S.view = S.groups.filter((g) => (tab === 'all' || g.tier === tab) && words.every((w) => g.lower.includes(w)));
  for (const b of $('tabs').children) b.classList.toggle('on', b.dataset.tab === tab);
  $('sizer').style.height = S.view.length * ROW + 'px';
  if (resetScroll) $('list').scrollTop = 0;

  const empty = $('empty');
  empty.hidden = S.view.length > 0;
  if (!S.view.length) {
    if (!S.faces.length) empty.innerHTML = '<div><big>폰트를 불러오는 중…</big></div>';
    else if (words.length) empty.innerHTML = `<div><big>"${esc(S.search)}" 검색 결과 없음</big>다른 탭이나 이름으로 찾아보세요.</div>`;
    else if (tab === 'full') empty.innerHTML = '<div><big>모든 글자를 지원하는 폰트가 없습니다</big>누락이 가장 적은 폰트부터 확인해 보세요.</div><button data-go="all">가장 근접한 폰트 보기</button>';
    else empty.innerHTML = '<div><big>해당하는 폰트가 없습니다</big></div>';
  }
  renderRows();
}

// ---------------------------------------------------------------- rendering
function useFont(f) {
  if (!f.ff) {
    f.ff = 'ff' + f.id;
    const q = (s) => s.replace(/[\\"]/g, '\\$&');
    const src = [`local("${q(f.full)}")`];
    if (f.ps && f.ps !== f.full) src.push(`local("${q(f.ps)}")`);
    src.push(`url("http://font.localhost/${f.id}")`);
    try { document.fonts.add(new FontFace(f.ff, src.join(', '))); } catch {}
  }
  return f.ff;
}

function markup(text, miss) {
  if (!miss || !miss.size) return esc(text);
  let out = '', plain = '';
  for (const ch of text) {
    const cp = ch.codePointAt(0);
    if (miss.has(cp)) {
      out += esc(plain) + `<mark class="${S.primarySet.has(cp) ? 'p' : 's'}">${esc(ch)}</mark>`;
      plain = '';
    } else plain += ch;
  }
  return out + esc(plain);
}

function missLine(g) {
  if (!S.allCps.length) return `<span>글자 ${nf.format(g.rep.cps)}개 수록</span>`;
  if (g.tier === 'full') return '<span class="okmsg">모든 글자 지원</span>';
  const miss = S.missing.get(g.rep.id);
  if (!miss) return '<span>…</span>';
  const arr = [...miss];
  const chars = arr.slice(0, 28).map((cp) => `<span class="${S.primarySet.has(cp) ? 'p' : 's'}">${esc(String.fromCodePoint(cp))}</span>`).join(' ');
  return `<span>누락 ${nf.format(arr.length)}자 </span>${chars}${arr.length > 28 ? ' …' : ''}`;
}

function rowHtml(g, i) {
  const ff = useFont(g.rep);
  const total = g.p + g.s;
  const badge = !S.allCps.length ? ['none', 'Aa'] : g.tier === 'full' ? ['ok', '✓'] : [g.tier === 'letters' ? 'warn' : 'bad', total > 99 ? '99+' : total];
  const styles = g.faces.length > 1 ? `${esc(g.rep.style)} 외 ${g.faces.length - 1}개` : esc(g.rep.style);
  return `<div class="row${S.sel === g ? ' sel' : ''}" data-i="${i}" style="top:${i * ROW}px">
    <div class="badge ${badge[0]}">${badge[1]}</div>
    <div class="name"><div class="fam" style="font-family:'${ff}',var(--ui)">${esc(g.family)}</div>
      <div class="sub">${g.rep.user ? '<em>추가됨</em>' : ''}${styles}</div></div>
    <div class="prev"><div class="line" style="font-family:'${ff}',var(--ui)">${markup(S.line, S.missing.get(g.rep.id))}</div>
      <div class="miss-list">${missLine(g)}</div></div>
  </div>`;
}

let range = [0, 0];
function renderRows() {
  const list = $('list');
  const from = Math.max(0, Math.floor(list.scrollTop / ROW) - 4);
  const to = Math.min(S.view.length, Math.ceil((list.scrollTop + list.clientHeight) / ROW) + 4);
  range = [from, to];
  let h = '';
  for (let i = from; i < to; i++) h += rowHtml(S.view[i], i);
  $('sizer').innerHTML = h;
  fetchMissing();
}

async function fetchMissing() {
  if (!S.allCps.length) return;
  const need = [];
  for (let i = range[0]; i < range[1]; i++) {
    const g = S.view[i];
    if (g.tier !== 'full' && !S.missing.has(g.rep.id)) need.push(g.rep.id);
  }
  if (!need.length) return;
  const gen = S.gen;
  const res = await invoke('missing', { ids: need, cps: S.allCps });
  if (gen !== S.gen) return;
  need.forEach((id, i) => S.missing.set(id, new Set(res[i])));
  renderRows();
}

// ------------------------------------------------------------------- drawer
async function openDrawer(g, face) {
  S.sel = g; S.selFace = face || g.rep;
  const f = S.selFace;
  const ff = useFont(f);
  $('drawer').hidden = false;
  $('dName').textContent = g.family;
  $('dName').style.fontFamily = `'${ff}', var(--ui)`;
  $('dMeta').textContent = `${f.path} · 글리프 ${nf.format(f.glyphs)} · 글자 ${nf.format(f.cps)}`;
  $('dMeta').title = f.path;
  $('dStyles').innerHTML = g.faces.map((x) => `<button data-id="${x.id}" class="${x === f ? 'on' : ''}">${esc(x.style)}</button>`).join('');
  renderRows();

  let miss = new Set();
  if (S.allCps.length && g.tier !== 'full') {
    miss = S.missing.get(f.id);
    if (!miss) {
      const gen = S.gen;
      const res = await invoke('missing', { ids: [f.id], cps: S.allCps });
      if (gen !== S.gen || S.selFace !== f) return;
      miss = new Set(res[0]);
      S.missing.set(f.id, miss);
    }
  }
  const v = $('dVerdict');
  if (!S.allCps.length) { v.className = 'verdict none'; v.textContent = '텍스트를 입력하면 이 폰트의 지원 여부를 보여줍니다'; }
  else if (g.tier === 'full') { v.className = 'verdict ok'; v.innerHTML = `✓ 모든 글자를 지원합니다<small>고유 글자 ${nf.format(S.allCps.length)}개</small>`; }
  else if (g.tier === 'letters') { v.className = 'verdict warn'; v.innerHTML = `글자는 모두 지원 · 기호 ${nf.format(g.s)}개 누락<small>노란색으로 표시</small>`; }
  else { v.className = 'verdict bad'; v.innerHTML = `글자 ${nf.format(g.p)}개${g.s ? ` · 기호 ${nf.format(g.s)}개` : ''} 누락<small>표시된 글자는 다른 폰트로 대체됩니다</small>`; }

  const arr = [...miss].sort((a, b) => (S.primarySet.has(b) - S.primarySet.has(a)) || a - b);
  $('dMissing').innerHTML = arr.slice(0, 400).map((cp) => `<div class="tile ${S.primarySet.has(cp) ? 'p' : 's'}"><b>${esc(String.fromCodePoint(cp))}</b><small>${hex(cp)}</small></div>`).join('');
  const body = S.text.trim() ? S.text.slice(0, 30000) : DEFAULT_LINE + '\nThe quick brown fox jumps over the lazy dog\n다람쥐 헌 쳇바퀴에 타고파\nいろはにほへと ちりぬるを';
  const p = $('dPreview');
  p.style.fontFamily = `'${ff}', var(--ui)`;
  p.innerHTML = markup(body, miss);
}

function closeDrawer() {
  S.sel = null; S.selFace = null;
  $('drawer').hidden = true;
  renderRows();
}

// ------------------------------------------------------------------- events
let timer;
$('text').addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(runQuery, 40); });
$('sample').onclick = () => { $('text').value = SAMPLE; runQuery(); };
$('clear').onclick = () => { $('text').value = ''; runQuery(); $('text').focus(); };
$('search').addEventListener('input', (e) => { S.search = e.target.value; applyFilter(true); });
$('tabs').onclick = (e) => { const b = e.target.closest('button'); if (b) { S.tab = b.dataset.tab; applyFilter(true); } };
$('empty').onclick = (e) => { if (e.target.dataset.go) { S.tab = e.target.dataset.go; applyFilter(true); } };
$('list').addEventListener('scroll', () => requestAnimationFrame(renderRows), { passive: true });
new ResizeObserver(() => renderRows()).observe($('list'));
$('sizer').onclick = (e) => { const r = e.target.closest('.row'); if (r) { const g = S.view[+r.dataset.i]; if (S.sel === g) closeDrawer(); else openDrawer(g); } };
$('dClose').onclick = closeDrawer;
$('dStyles').onclick = (e) => { const b = e.target.closest('button'); if (b) openDrawer(S.sel, S.faces[+b.dataset.id]); };
$('dSize').oninput = (e) => { $('dPreview').style.fontSize = e.target.value + 'px'; };
$('dReveal').onclick = () => invoke('reveal', { path: S.selFace.path });
$('dCopy').onclick = async () => {
  const name = S.sel.family;
  try { await navigator.clipboard.writeText(name); }
  catch { const t = document.createElement('textarea'); t.value = name; document.body.append(t); t.select(); document.execCommand('copy'); t.remove(); }
  toast(`"${name}" 복사됨`);
};
$('addFolder').onclick = async () => { const r = await window.__TAURI__.dialog.open({ directory: true, multiple: true, title: '폰트 폴더 선택' }); if (r) addSources([].concat(r)); };
$('addFiles').onclick = async () => {
  const r = await window.__TAURI__.dialog.open({ multiple: true, title: '폰트 파일 선택', filters: [{ name: '폰트', extensions: ['ttf', 'otf', 'ttc', 'otc'] }] });
  if (r) addSources([].concat(r));
};
$('rescan').onclick = () => loadLibrary(true);
$('extraChip').onclick = async (e) => { if (e.target.closest('button')) { await invoke('clear_sources'); await loadLibrary(); toast('추가한 경로를 모두 제거했습니다'); } };
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && S.sel) closeDrawer();
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') { e.preventDefault(); $('search').focus(); $('search').select(); }
});
document.addEventListener('contextmenu', (e) => { if (!e.target.closest('textarea, input, .d-preview, .d-missing, .d-meta')) e.preventDefault(); });

listen('scan-progress', (e) => { const [done, total] = e.payload; $('progress').firstElementChild.style.width = (done / total) * 100 + '%'; });
listen('tauri://drag-enter', () => $('drop').classList.add('on'));
listen('tauri://drag-leave', () => $('drop').classList.remove('on'));
listen('tauri://drag-drop', async (e) => {
  $('drop').classList.remove('on');
  const paths = e.payload.paths || [];
  const texts = paths.filter((p) => TEXT_EXT.test(p));
  const others = paths.filter((p) => !TEXT_EXT.test(p));
  if (texts.length) {
    const parts = [];
    for (const p of texts) { try { parts.push(await invoke('read_text', { path: p })); } catch (err) { toast(String(err)); } }
    if (parts.length) { $('text').value = parts.join('\n'); runQuery(); }
  }
  if (others.length) addSources(others);
});

try { $('text').value = localStorage.getItem('fonter.text') || ''; } catch {}
renderStats(analyze(''));
loadLibrary().catch((err) => { $('libStatus').textContent = '오류: ' + err; });

use fonter_core::*;
use std::collections::HashMap;
use std::time::Instant;

const TEXT: &str = "夜に駆ける 沈むように溶けてゆくように 二人だけの空が広がる夜に\n「さよなら」だけだった その一言で全てが分かった\n日が沈み出した空と君の姿 フェンス越しに重なっていた\n初めて会った日から 僕の心の全てを奪った\n騒がしい日々に笑えない君に 思い付く限り眩しい明日を\n後來 我總算學會了如何去愛 可惜你早已遠去 消失在人海\n那些年错过的大雨 那些年错过的爱情 好想拥抱你\n밤하늘의 별을 따서 너에게 줄래 너는 내가 사랑하니까 더 소중하니까\nStay with me, 真夜中のドアをたたき 帰らないでと泣いた ♪ ～ … ♡";

fn ms(t: Instant) -> f64 { t.elapsed().as_secs_f64() * 1000.0 }

fn main() {
    let t = Instant::now();
    let files = list_font_files(&system_font_dirs());
    let bytes: u64 = files.iter().map(|f| f.size).sum();
    println!("list            {:>9.1} ms  ({} files, {:.0} MB)", ms(t), files.len(), bytes as f64 / 1e6);

    let t = Instant::now();
    let mut n = 0;
    for f in &files { if let Ok(d) = std::fs::read(&f.path) { n += parse_bytes(&d, &f.path).len(); } }
    println!("read-all, 1 thr {:>9.1} ms  ({} faces)", ms(t), n);

    let t = Instant::now();
    let mut n = 0;
    for f in &files { n += parse_file(&f.path).len(); }
    println!("mmap, 1 thread  {:>9.1} ms  ({} faces)", ms(t), n);

    let t = Instant::now();
    let (entries, _) = scan(&files, &HashMap::new(), &|_, _| {});
    println!("mmap, parallel  {:>9.1} ms", ms(t));

    let cache_path = std::env::temp_dir().join("fonter-bench.idx");
    let t = Instant::now();
    save_cache(&cache_path, &entries).unwrap();
    println!("cache save      {:>9.1} ms  ({:.1} MB)", ms(t), std::fs::metadata(&cache_path).unwrap().len() as f64 / 1e6);
    let t = Instant::now();
    let cache = load_cache(&cache_path);
    let (entries, parsed) = scan(&files, &cache, &|_, _| {});
    println!("cache load+stat {:>9.1} ms  (re-parsed {})", ms(t), parsed);

    let t = Instant::now();
    let index = Index::build(entries);
    println!("index build     {:>9.1} ms  ({} faces)", ms(t), index.faces.len());

    let mut cps: Vec<u32> = TEXT.chars().filter(|c| !c.is_whitespace()).map(|c| c as u32).collect();
    cps.sort_unstable(); cps.dedup();
    println!("query: {} unique code points x {} faces", cps.len(), index.faces.len());
    const R: u32 = 200;

    let t = Instant::now();
    let mut full = 0;
    for _ in 0..R { full = index.faces.iter().filter(|f| cps.iter().all(|&c| ranges_have(&f.ranges, c))).count(); }
    println!("A ranges bsearch, full-only   {:>8.1} us  ({} full)", ms(t) * 1000.0 / R as f64, full);

    let t = Instant::now();
    for _ in 0..R { full = index.faces.iter().map(|f| cps.iter().filter(|&&c| !ranges_have(&f.ranges, c)).count()).filter(|&m| m == 0).count(); }
    println!("A ranges bsearch, counts      {:>8.1} us  ({} full)", ms(t) * 1000.0 / R as f64, full);

    let t = Instant::now();
    for _ in 0..R { full = index.covers.iter().map(|c| cps.iter().filter(|&&cp| !c.has(cp)).count()).filter(|&m| m == 0).count(); }
    println!("B paged bitset, counts, 1 thr {:>8.1} us  ({} full)", ms(t) * 1000.0 / R as f64, full);

    let t = Instant::now();
    for _ in 0..R { full = index.query(&cps, &[]).iter().filter(|m| m.0 == 0).count(); }
    println!("B paged bitset, counts, auto  {:>8.1} us  ({} full)", ms(t) * 1000.0 / R as f64, full);

    // C: inverted rows (code point -> bitset of faces), AND-ed together.
    let words = (index.faces.len() + 63) / 64;
    let t = Instant::now();
    let rows: Vec<Vec<u64>> = cps.iter().map(|&cp| {
        let mut row = vec![0u64; words];
        for (i, c) in index.covers.iter().enumerate() { if c.has(cp) { row[i / 64] |= 1 << (i % 64); } }
        row
    }).collect();
    let build = ms(t) * 1000.0;
    let t = Instant::now();
    for _ in 0..R {
        let mut acc = vec![!0u64; words];
        for r in &rows { for (a, b) in acc.iter_mut().zip(r) { *a &= b; } }
        full = acc.iter().map(|w| w.count_ones() as usize).sum::<usize>();
    }
    println!("C inverted rows, full-only    {:>8.1} us  ({} full; rows built in {:.0} us)", ms(t) * 1000.0 / R as f64, full, build);
}

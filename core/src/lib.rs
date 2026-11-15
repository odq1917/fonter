//! Fonter core: font discovery, cmap coverage extraction, cached index, queries.
//!
//! Speed comes from three things:
//! 1. Files are memory-mapped and only the `cmap`, `name`, `OS/2` and `maxp`
//!    tables are touched, so a 20 MB CJK font costs a few page faults.
//! 2. Files are parsed in parallel and the result is cached on disk, keyed by
//!    path + size + mtime, so later launches only re-parse what changed.
//! 3. Coverage is stored as a three-level paged bitset, so "does face F have
//!    code point C" is three array loads and a whole-library query is sub-ms.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use ttf_parser::{name_id, PlatformId, RawFace, Tag};

const CACHE_MAGIC: u32 = 0x464E_5435; // "FNT5"
const FONT_EXTS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FaceInfo {
    pub path: String,
    pub index: u32,
    /// Display family name (Korean name preferred when the font has one).
    pub family: String,
    pub style: String,
    /// English full name and PostScript name, used to address the installed face.
    pub full_name: String,
    pub ps_name: String,
    pub weight: u16,
    pub italic: bool,
    pub glyphs: u16,
    pub cp_count: u32,
    /// Inclusive code point ranges, sorted and merged.
    pub ranges: Vec<(u32, u32)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub faces: Vec<FaceInfo>,
}

#[derive(Clone, Debug)]
pub struct FontFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
}

// ---------------------------------------------------------------- discovery

pub fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    dirs.push(PathBuf::from(windir).join("Fonts"));
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft\\Windows\\Fonts"));
    }
    dirs
}

pub fn is_font_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| FONT_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
        .unwrap_or(false)
}

fn stat(path: PathBuf) -> Option<FontFile> {
    let md = fs::metadata(&path).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(FontFile { path, size: md.len(), mtime })
}

/// Collects font files from the given files and directories (recursive).
pub fn list_font_files(roots: &[PathBuf]) -> Vec<FontFile> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, u32)> = roots.iter().map(|p| (p.clone(), 0)).collect();
    while let Some((p, depth)) = stack.pop() {
        if p.is_dir() {
            if depth > 8 {
                continue;
            }
            if let Ok(rd) = fs::read_dir(&p) {
                for e in rd.flatten() {
                    let child = e.path();
                    match e.file_type() {
                        Ok(t) if t.is_dir() => stack.push((child, depth + 1)),
                        Ok(_) if is_font_path(&child) => out.extend(stat(child)),
                        _ => {}
                    }
                }
            }
        } else if is_font_path(&p) {
            out.extend(stat(p));
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

// ------------------------------------------------------------------ parsing

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn decode_name(n: &ttf_parser::name::Name) -> Option<String> {
    let unicode = match n.platform_id {
        PlatformId::Unicode => true,
        PlatformId::Windows => n.encoding_id == 1 || n.encoding_id == 10,
        _ => false,
    };
    let s = if unicode {
        let units: Vec<u16> = n.name.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else if n.platform_id == PlatformId::Macintosh && n.encoding_id == 0 {
        n.name.iter().map(|&b| if b < 0x80 { b as char } else { '?' }).collect()
    } else {
        return None;
    };
    let s = s.trim().trim_matches('\0').to_string();
    if s.is_empty() { None } else { Some(s) }
}

struct Names {
    // (rank, value): lower rank wins.
    slots: HashMap<(u16, bool), (u8, String)>,
}

impl Names {
    fn read(raw: &RawFace) -> Names {
        let mut slots: HashMap<(u16, bool), (u8, String)> = HashMap::new();
        let Some(table) = raw.table(Tag::from_bytes(b"name")).and_then(ttf_parser::name::Table::parse) else {
            return Names { slots };
        };
        for n in table.names {
            let Some(s) = decode_name(&n) else { continue };
            let win = n.platform_id == PlatformId::Windows;
            let korean = win && n.language_id == 0x0412;
            let english = (win && n.language_id == 0x0409) || n.platform_id == PlatformId::Macintosh;
            // localized slot: Korean > English > anything; english slot: English > anything.
            let loc_rank = if korean { 0 } else if english && win { 1 } else if english { 2 } else { 3 };
            let en_rank = if english && win { 0 } else if english { 1 } else if !win { 2 } else { 3 };
            for (key, rank) in [((n.name_id, true), loc_rank), ((n.name_id, false), en_rank)] {
                let better = slots.get(&key).map_or(true, |(r, _)| rank < *r);
                if better {
                    slots.insert(key, (rank, s.clone()));
                }
            }
        }
        Names { slots }
    }
    fn get(&self, id: u16, localized: bool) -> Option<String> {
        self.slots.get(&(id, localized)).map(|(_, s)| s.clone())
    }
}

fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

/// Detects glyphs that exist in `cmap` but draw nothing. Some fonts map whole
/// blocks (typically Hanja) to empty placeholder glyphs, which would otherwise
/// be reported as supported and then render as blank space.
enum Blanks<'a> {
    Unknown,
    Glyf { loca: &'a [u8], long: bool, glyf: &'a [u8] },
    Cff { offsets: &'a [u8], off_size: usize, data: &'a [u8], count: usize, table: Option<ttf_parser::cff::Table<'a>> },
}

/// Outline sink that only records whether anything was drawn.
struct Ink(bool);

impl ttf_parser::OutlineBuilder for Ink {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) { self.0 = true; }
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) { self.0 = true; }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) { self.0 = true; }
    fn close(&mut self) {}
}

impl<'a> Blanks<'a> {
    fn new(raw: &RawFace<'a>) -> Blanks<'a> {
        if let (Some(loca), Some(head)) = (raw.table(Tag::from_bytes(b"loca")), raw.table(Tag::from_bytes(b"head"))) {
            let glyf = raw.table(Tag::from_bytes(b"glyf")).unwrap_or(&[]);
            return Blanks::Glyf { loca, long: be16(head, 50) == Some(1), glyf };
        }
        raw.table(Tag::from_bytes(b"CFF ")).and_then(Self::cff).unwrap_or(Blanks::Unknown)
    }

    /// Returns (offset array, offSize, object data, count, end position) of a CFF INDEX.
    fn cff_index(d: &'a [u8], at: usize) -> Option<(&'a [u8], usize, &'a [u8], usize, usize)> {
        let count = be16(d, at)? as usize;
        if count == 0 {
            return Some((&[], 1, &[], 0, at + 2));
        }
        let off_size = *d.get(at + 2)? as usize;
        if !(1..=4).contains(&off_size) {
            return None;
        }
        let offsets = d.get(at + 3..at + 3 + (count + 1) * off_size)?;
        let data_start = at + 3 + (count + 1) * off_size;
        let last = Self::cff_offset(offsets, off_size, count)?;
        let data = d.get(data_start..data_start + last.checked_sub(1)?)?;
        Some((offsets, off_size, data, count, data_start + last - 1))
    }

    fn cff_offset(offsets: &[u8], off_size: usize, i: usize) -> Option<usize> {
        let b = offsets.get(i * off_size..(i + 1) * off_size)?;
        Some(b.iter().fold(0usize, |acc, &x| (acc << 8) | x as usize))
    }

    fn cff(d: &'a [u8]) -> Option<Blanks<'a>> {
        if *d.first()? != 1 {
            return None; // CFF2 and unknown versions: no detection
        }
        let (.., after_names) = Self::cff_index(d, *d.get(2)? as usize)?;
        let (offsets, off_size, dicts, count, _) = Self::cff_index(d, after_names)?;
        if count == 0 {
            return None;
        }
        let top = dicts.get(..Self::cff_offset(offsets, off_size, 1)?.checked_sub(1)?)?;
        // Walk the Top DICT for operator 17 (CharStrings offset).
        let (mut i, mut last, mut charstrings) = (0usize, 0i64, None);
        while i < top.len() {
            let b = top[i];
            match b {
                32..=246 => { last = b as i64 - 139; i += 1; }
                247..=250 => { last = (b as i64 - 247) * 256 + *top.get(i + 1)? as i64 + 108; i += 2; }
                251..=254 => { last = -(b as i64 - 251) * 256 - *top.get(i + 1)? as i64 - 108; i += 2; }
                28 => { last = be16(top, i + 1)? as i16 as i64; i += 3; }
                29 => { last = be32(top, i + 1)? as i32 as i64; i += 5; }
                30 => {
                    i += 1;
                    loop {
                        let n = *top.get(i)?;
                        i += 1;
                        if n & 0x0F == 0x0F || n >> 4 == 0x0F { break; }
                    }
                }
                12 => i += 2,
                17 => { charstrings = Some(last); i += 1; }
                _ => i += 1,
            }
        }
        let at = usize::try_from(charstrings?).ok()?;
        let (offsets, off_size, data, count, _) = Self::cff_index(d, at)?;
        Some(Blanks::Cff { offsets, off_size, data, count, table: ttf_parser::cff::Table::parse(d) })
    }

    fn is_blank(&self, glyph: u16) -> bool {
        let g = glyph as usize;
        match *self {
            Blanks::Unknown => false,
            Blanks::Glyf { loca, long, glyf } => {
                let at = |i: usize| if long { be32(loca, i * 4) } else { be16(loca, i * 2).map(|v| u32::from(v) * 2) };
                match (at(g), at(g + 1)) {
                    (Some(a), Some(b)) if a == b => true,
                    // A header-only record that declares zero contours draws nothing either.
                    // Longer records are not inspected, so the glyf table stays unread.
                    (Some(a), Some(b)) if b > a && b - a <= 12 => be16(glyf, a as usize) == Some(0),
                    _ => false,
                }
            }
            Blanks::Cff { offsets, off_size, data, count, ref table } => {
                if g >= count {
                    return false;
                }
                let (Some(a), Some(b)) = (Self::cff_offset(offsets, off_size, g), Self::cff_offset(offsets, off_size, g + 1)) else { return false };
                let Some(cs) = data.get(a.saturating_sub(1)..b.saturating_sub(1)) else { return false };
                // Blank = nothing but an optional width followed by `endchar`.
                match cs {
                    [] | [14] => true,
                    [body @ .., 14] if body.len() <= 5 => {
                        let n = match body[0] { 32..=246 => 1, 247..=254 => 2, 28 => 3, 255 => 5, _ => 0 };
                        n == body.len()
                    }
                    // Short programs can still be blank through a subroutine call
                    // (e.g. `-84 callsubr`), so those are actually executed.
                    _ if cs.len() <= 12 => table.as_ref().map_or(false, |t| {
                        let mut ink = Ink(false);
                        let result = t.outline(ttf_parser::GlyphId(glyph), &mut ink);
                        // The error type is not exported; "ZeroBBox" is how ttf-parser reports an empty outline.
                        !ink.0 && result.map_or_else(|e| format!("{:?}", e) == "ZeroBBox", |_| true)
                    }),
                    _ => false,
                }
            }
        }
    }
}

/// Code points that are legitimately invisible and must stay "supported".
fn invisible_ok(cp: u32) -> bool {
    matches!(cp, 0x115F | 0x1160 | 0x2800 | 0x3164 | 0xFFA0)
        || char::from_u32(cp).map_or(true, |c| c.is_whitespace() || c.is_control())
        || matches!(cp, 0x00AD | 0x034F | 0x061C | 0x180B..=0x180F | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFE00..=0xFE0F | 0xFEFF | 0xE0000..=0xE0FFF)
}

/// Walks a format 4 (BMP) subtable segment by segment.
fn walk_format4(t: &[u8], mut emit: impl FnMut(u32, u16)) -> Option<()> {
    let seg2 = be16(t, 6)? as usize;
    let (ends, starts, deltas, offsets) = (14, 16 + seg2, 16 + 2 * seg2, 16 + 3 * seg2);
    for i in (0..seg2).step_by(2) {
        let (end, start) = (be16(t, ends + i)? as u32, be16(t, starts + i)? as u32);
        let (delta, offset) = (be16(t, deltas + i)?, be16(t, offsets + i)? as usize);
        if start > end {
            continue;
        }
        for cp in start..=end.min(0xFFFE) {
            let glyph = if offset == 0 {
                (cp as u16).wrapping_add(delta)
            } else {
                let raw = be16(t, offsets + i + offset + 2 * (cp - start) as usize).unwrap_or(0);
                if raw == 0 { 0 } else { raw.wrapping_add(delta) }
            };
            emit(cp, glyph);
        }
    }
    Some(())
}

/// Walks a format 12 (full repertoire) subtable group by group.
fn walk_format12(t: &[u8], mut emit: impl FnMut(u32, u16)) -> Option<()> {
    let groups = be32(t, 12)? as usize;
    for g in 0..groups.min(t.len() / 12) {
        let at = 16 + g * 12;
        let (start, end, first) = (be32(t, at)?, be32(t, at + 4)?.min(0x10FFFF), be32(t, at + 8)?);
        if start > end {
            continue;
        }
        for cp in start..=end {
            let glyph = first.wrapping_add(cp - start);
            if glyph > 0xFFFF {
                break;
            }
            emit(cp, glyph as u16);
        }
    }
    Some(())
}

fn coverage(raw: &RawFace) -> Vec<(u32, u32)> {
    let blanks = Blanks::new(raw);
    let Some(table) = raw.table(Tag::from_bytes(b"cmap")) else { return Vec::new() };
    let mut cps: Vec<u32> = Vec::new();
    let mut emit = |cp: u32, glyph: u16| {
        if glyph != 0 && (!blanks.is_blank(glyph) || invisible_ok(cp)) {
            cps.push(cp);
        }
    };

    // Pick the best Unicode subtable: full repertoire (format 12) over BMP (format 4).
    let mut best: Option<(u8, &[u8])> = None;
    for i in 0..be16(table, 2).unwrap_or(0) as usize {
        let rec = 4 + i * 8;
        let (Some(platform), Some(encoding), Some(offset)) = (be16(table, rec), be16(table, rec + 2), be32(table, rec + 4)) else { break };
        let Some(sub) = table.get(offset as usize..) else { continue };
        let rank = match (platform, encoding, be16(sub, 0)) {
            (3, 10, Some(12)) | (0, 4, Some(12)) | (0, 6, Some(12)) => 2,
            (3, 1, Some(4)) | (0, 0..=4, Some(4)) => 1,
            _ => continue,
        };
        if best.map_or(true, |(r, _)| rank > r) {
            best = Some((rank, sub));
        }
    }
    match best {
        Some((2, sub)) => { walk_format12(sub, &mut emit); }
        Some((_, sub)) => { walk_format4(sub, &mut emit); }
        None => {
            // Rare formats (0, 6, 10, 13): let ttf-parser handle them.
            if let Some(cmap) = ttf_parser::cmap::Table::parse(table) {
                for s in cmap.subtables.into_iter().filter(|s| s.is_unicode()) {
                    s.codepoints(|cp| emit(cp, s.glyph_index(cp).map_or(0, |g| g.0)));
                }
            }
        }
    }

    cps.sort_unstable();
    cps.dedup();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for cp in cps {
        match ranges.last_mut() {
            Some(last) if last.1 + 1 == cp => last.1 = cp,
            _ => ranges.push((cp, cp)),
        }
    }
    ranges
}

fn parse_face(data: &[u8], index: u32, path: &Path) -> Option<FaceInfo> {
    let raw = RawFace::parse(data, index).ok()?;
    let names = Names::read(&raw);
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

    let family = names
        .get(name_id::TYPOGRAPHIC_FAMILY, true)
        .or_else(|| names.get(name_id::FAMILY, true))
        .unwrap_or_else(|| stem.clone());
    let style = names
        .get(name_id::TYPOGRAPHIC_SUBFAMILY, false)
        .or_else(|| names.get(name_id::SUBFAMILY, false))
        .unwrap_or_else(|| "Regular".into());
    let full_name = names.get(name_id::FULL_NAME, false).unwrap_or_else(|| family.clone());
    let ps_name = names.get(name_id::POST_SCRIPT_NAME, false).unwrap_or_default();

    let (mut weight, mut italic) = (400u16, false);
    if let Some(os2) = raw.table(Tag::from_bytes(b"OS/2")) {
        weight = be16(os2, 4).unwrap_or(400);
        italic = be16(os2, 62).map_or(false, |sel| sel & 1 != 0);
    }
    let glyphs = raw.table(Tag::from_bytes(b"maxp")).and_then(|t| be16(t, 4)).unwrap_or(0);

    let ranges = coverage(&raw);
    let cp_count = ranges.iter().map(|(a, b)| b - a + 1).sum();
    Some(FaceInfo {
        path: path.to_string_lossy().to_string(),
        index,
        family,
        style,
        full_name,
        ps_name,
        weight,
        italic,
        glyphs,
        cp_count,
        ranges,
    })
}

/// Parses every face in a font file. Unreadable or broken files yield no faces.
pub fn parse_file(path: &Path) -> Vec<FaceInfo> {
    let Ok(file) = fs::File::open(path) else { return Vec::new() };
    // Safety: the map is read-only and dropped before returning.
    let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else { return Vec::new() };
    parse_bytes(&map, path)
}

pub fn parse_bytes(data: &[u8], path: &Path) -> Vec<FaceInfo> {
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1).min(256);
    (0..count).filter_map(|i| parse_face(data, i, path)).collect()
}

// -------------------------------------------------------------------- cache

pub fn load_cache(path: &Path) -> HashMap<String, FileEntry> {
    let Ok(bytes) = fs::read(path) else { return HashMap::new() };
    if bytes.len() < 4 || bytes[..4] != CACHE_MAGIC.to_le_bytes() {
        return HashMap::new();
    }
    let entries: Vec<FileEntry> = bincode::deserialize(&bytes[4..]).unwrap_or_default();
    entries.into_iter().map(|e| (e.path.clone(), e)).collect()
}

pub fn save_cache(path: &Path, entries: &[FileEntry]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut bytes = CACHE_MAGIC.to_le_bytes().to_vec();
    bincode::serialize_into(&mut bytes, entries).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

/// Parses `files` in parallel, reusing cached entries whose size and mtime match.
/// Returns the entries and how many files were actually parsed.
pub fn scan(
    files: &[FontFile],
    cache: &HashMap<String, FileEntry>,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> (Vec<FileEntry>, usize) {
    let done = std::sync::atomic::AtomicUsize::new(0);
    let parsed = std::sync::atomic::AtomicUsize::new(0);
    let total = files.len();
    let entries = files
        .par_iter()
        .map(|f| {
            let key = f.path.to_string_lossy().to_string();
            let entry = match cache.get(&key) {
                Some(c) if c.size == f.size && c.mtime == f.mtime => c.clone(),
                _ => {
                    parsed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    FileEntry { path: key, size: f.size, mtime: f.mtime, faces: parse_file(&f.path) }
                }
            };
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n % 16 == 0 || n == total {
                progress(n, total);
            }
            entry
        })
        .collect();
    (entries, parsed.into_inner())
}

// -------------------------------------------------------------------- index

const NONE: u16 = u16::MAX;

/// Three-level paged bitset over the Unicode code space:
/// plane (17) -> 256-code-point page table -> 256-bit page.
pub struct Cover {
    planes: [u16; 17],
    tables: Vec<[u16; 256]>,
    pages: Vec<[u64; 4]>,
}

impl Cover {
    pub fn from_ranges(ranges: &[(u32, u32)]) -> Cover {
        let mut c = Cover { planes: [NONE; 17], tables: Vec::new(), pages: Vec::new() };
        for &(a, b) in ranges {
            for cp in a..=b.min(0x10FFFF) {
                let plane = (cp >> 16) as usize;
                if c.planes[plane] == NONE {
                    c.planes[plane] = c.tables.len() as u16;
                    c.tables.push([NONE; 256]);
                }
                let t = c.planes[plane] as usize;
                let slot = ((cp >> 8) & 0xFF) as usize;
                if c.tables[t][slot] == NONE {
                    c.tables[t][slot] = c.pages.len() as u16;
                    c.pages.push([0; 4]);
                }
                let p = c.tables[t][slot] as usize;
                c.pages[p][((cp >> 6) & 3) as usize] |= 1u64 << (cp & 63);
            }
        }
        c
    }

    #[inline]
    pub fn has(&self, cp: u32) -> bool {
        let Some(&t) = self.planes.get((cp >> 16) as usize) else { return false };
        if t == NONE {
            return false;
        }
        let p = self.tables[t as usize][((cp >> 8) & 0xFF) as usize];
        if p == NONE {
            return false;
        }
        self.pages[p as usize][((cp >> 6) & 3) as usize] >> (cp & 63) & 1 != 0
    }
}

pub struct Index {
    pub faces: Vec<FaceInfo>,
    pub covers: Vec<Cover>,
}

impl Index {
    pub fn build(entries: Vec<FileEntry>) -> Index {
        let faces: Vec<FaceInfo> = entries.into_iter().flat_map(|e| e.faces).collect();
        let covers = faces.par_iter().map(|f| Cover::from_ranges(&f.ranges)).collect();
        Index { faces, covers }
    }

    /// For every face: how many of `primary` and `secondary` it lacks.
    pub fn query(&self, primary: &[u32], secondary: &[u32]) -> Vec<(u32, u32)> {
        let one = |c: &Cover| {
            let p = primary.iter().filter(|&&cp| !c.has(cp)).count() as u32;
            let s = secondary.iter().filter(|&&cp| !c.has(cp)).count() as u32;
            (p, s)
        };
        if self.covers.len() * (primary.len() + secondary.len()) < 200_000 {
            self.covers.iter().map(one).collect()
        } else {
            self.covers.par_iter().map(one).collect()
        }
    }

    pub fn missing(&self, face: usize, cps: &[u32]) -> Vec<u32> {
        match self.covers.get(face) {
            Some(c) => cps.iter().copied().filter(|&cp| !c.has(cp)).collect(),
            None => Vec::new(),
        }
    }
}

/// Reference lookup used by the benchmark: binary search over sorted ranges.
pub fn ranges_have(ranges: &[(u32, u32)], cp: u32) -> bool {
    let i = ranges.partition_point(|r| r.1 < cp);
    i < ranges.len() && ranges[i].0 <= cp
}

// ------------------------------------------------------------ face extraction

/// Returns a standalone sfnt for one face. Single-face files are returned as is;
/// collection members are rebuilt, since browsers cannot load a .ttc directly.
pub fn face_bytes(path: &Path, index: u32) -> Option<Vec<u8>> {
    let data = fs::read(path).ok()?;
    if data.get(..4)? != b"ttcf" {
        return Some(data);
    }
    let be32 = |o: usize| -> Option<u32> { Some(u32::from_be_bytes(data.get(o..o + 4)?.try_into().ok()?)) };
    let count = be32(8)?;
    if index >= count {
        return None;
    }
    let base = be32(12 + 4 * index as usize)? as usize;
    let num = be16(&data, base + 4)? as usize;
    let header = 12 + 16 * num;
    let mut out = Vec::with_capacity(header);
    out.extend_from_slice(data.get(base..base + 12)?);
    out.resize(header, 0);
    for i in 0..num {
        let rec = base + 12 + 16 * i;
        let (off, len) = (be32(rec + 8)? as usize, be32(rec + 12)? as usize);
        let body = data.get(off..off.checked_add(len)?)?;
        let new_off = out.len() as u32;
        out.extend_from_slice(body);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let dst = 12 + 16 * i;
        out[dst..dst + 8].copy_from_slice(data.get(rec..rec + 8)?);
        out[dst + 8..dst + 12].copy_from_slice(&new_off.to_be_bytes());
        out[dst + 12..dst + 16].copy_from_slice(&(len as u32).to_be_bytes());
    }
    Some(out)
}

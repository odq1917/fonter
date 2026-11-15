#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fonter_core as core;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    index: RwLock<core::Index>,
    /// Extra files and folders the user added on top of the system fonts.
    extra: Mutex<Vec<PathBuf>>,
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Fonter")
}

fn load_extra() -> Vec<PathBuf> {
    std::fs::read_to_string(data_dir().join("sources.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<PathBuf>>(&s).ok())
        .unwrap_or_default()
}

fn save_extra(extra: &[PathBuf]) {
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(s) = serde_json::to_string(extra) {
        let _ = std::fs::write(data_dir().join("sources.json"), s);
    }
}

#[derive(Serialize)]
struct FaceDto {
    id: u32,
    family: String,
    style: String,
    full: String,
    ps: String,
    weight: u16,
    italic: bool,
    path: String,
    cps: u32,
    glyphs: u16,
    user: bool,
}

#[derive(Serialize)]
struct Library {
    faces: Vec<FaceDto>,
    files: usize,
    parsed: usize,
    ms: f64,
    extra: Vec<String>,
}

#[tauri::command(async)]
fn load_library(app: AppHandle, state: State<AppState>, fresh: bool) -> Library {
    let t = Instant::now();
    let extra = state.extra.lock().unwrap().clone();
    let system = core::system_font_dirs();
    let mut roots = system.clone();
    roots.extend(extra.iter().cloned());

    let files = core::list_font_files(&roots);
    let cache_path = data_dir().join("index.bin");
    let cache = if fresh { Default::default() } else { core::load_cache(&cache_path) };
    let (entries, parsed) = core::scan(&files, &cache, &|done, total| {
        let _ = app.emit("scan-progress", (done, total));
    });
    if parsed > 0 || entries.len() != cache.len() {
        let _ = core::save_cache(&cache_path, &entries);
    }
    let index = core::Index::build(entries);

    let faces = index
        .faces
        .iter()
        .enumerate()
        .map(|(i, f)| FaceDto {
            id: i as u32,
            family: f.family.clone(),
            style: f.style.clone(),
            full: f.full_name.clone(),
            ps: f.ps_name.clone(),
            weight: f.weight,
            italic: f.italic,
            path: f.path.clone(),
            cps: f.cp_count,
            glyphs: f.glyphs,
            user: !system.iter().any(|d| Path::new(&f.path).starts_with(d)),
        })
        .collect();
    *state.index.write().unwrap() = index;
    Library {
        faces,
        files: files.len(),
        parsed,
        ms: t.elapsed().as_secs_f64() * 1000.0,
        extra: extra.iter().map(|p| p.to_string_lossy().to_string()).collect(),
    }
}

#[tauri::command(async)]
fn add_sources(state: State<AppState>, paths: Vec<String>) -> usize {
    let mut extra = state.extra.lock().unwrap();
    let mut added = 0;
    for p in paths.into_iter().map(PathBuf::from) {
        if (p.is_dir() || core::is_font_path(&p)) && !extra.contains(&p) {
            extra.push(p);
            added += 1;
        }
    }
    save_extra(&extra);
    added
}

#[tauri::command(async)]
fn clear_sources(state: State<AppState>) {
    let mut extra = state.extra.lock().unwrap();
    extra.clear();
    save_extra(&extra);
}

#[derive(Serialize)]
struct QueryResult {
    /// Flat [missing_primary, missing_secondary] per face.
    counts: Vec<u32>,
    micros: f64,
}

#[tauri::command(async)]
fn query(state: State<AppState>, primary: Vec<u32>, secondary: Vec<u32>) -> QueryResult {
    let index = state.index.read().unwrap();
    let t = Instant::now();
    let res = index.query(&primary, &secondary);
    let micros = t.elapsed().as_secs_f64() * 1e6;
    QueryResult { counts: res.into_iter().flat_map(|(p, s)| [p, s]).collect(), micros }
}

#[tauri::command(async)]
fn missing(state: State<AppState>, ids: Vec<u32>, cps: Vec<u32>) -> Vec<Vec<u32>> {
    let index = state.index.read().unwrap();
    ids.iter().map(|&id| index.missing(id as usize, &cps)).collect()
}

#[tauri::command(async)]
fn reveal(path: String) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path)).spawn();
}

/// Reads a dropped text file (lyrics, subtitles). UTF-8 and UTF-16 with BOM.
#[tauri::command(async)]
fn read_text(path: String) -> Result<String, String> {
    let md = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if md.len() > 8 * 1024 * 1024 {
        return Err("파일이 너무 큽니다 (8MB 초과)".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let utf16 = |le: bool| {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    Ok(match bytes.as_slice() {
        [0xFF, 0xFE, ..] => utf16(true),
        [0xFE, 0xFF, ..] => utf16(false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(&bytes).into_owned(),
    })
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            index: RwLock::new(core::Index::build(Vec::new())),
            extra: Mutex::new(load_extra()),
        })
        // http://font.localhost/<face id> serves one face as a standalone font,
        // for previewing fonts that are not installed.
        .register_asynchronous_uri_scheme_protocol("font", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || {
                let id = request.uri().path().trim_matches('/').parse::<usize>().ok();
                let target = id.and_then(|id| {
                    let state = app.state::<AppState>();
                    let index = state.index.read().unwrap();
                    index.faces.get(id).map(|f| (PathBuf::from(&f.path), f.index))
                });
                let body = target.and_then(|(path, index)| core::face_bytes(&path, index));
                let builder = tauri::http::Response::builder().header("Access-Control-Allow-Origin", "*");
                let response = match body {
                    Some(bytes) => builder.status(200).header("Content-Type", "font/sfnt").body(bytes),
                    None => builder.status(404).body(Vec::new()),
                };
                responder.respond(response.unwrap());
            });
        })
        .invoke_handler(tauri::generate_handler![
            load_library,
            add_sources,
            clear_sources,
            query,
            missing,
            reveal,
            read_text
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Fonter");
}

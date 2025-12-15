use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::Emitter;
use tauri::Manager;

const SCAN_PROGRESS_EVENT: &str = "rustreader_scan_progress";
const APP_PREFIX: &str = "rustreader";
const APP_TITLE_PREFIX: &str = "rustreader - ";
const RECENT_LIMIT_DEFAULT: usize = 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppConfig {
  #[serde(skip_serializing_if = "Option::is_none")]
  language: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  font_size_px: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProgressEvent {
  scan_id: Option<String>,
  stage: &'static str,
  scanned_dirs: u64,
  scanned_files: u64,
  matched_files: u64,
  current_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanFile {
  virtual_path: String,
  abs_path: String,
  category: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanResult {
  root: String,
  label: String,
  files: Vec<ScanFile>,
}

fn home_dir() -> Option<PathBuf> {
  if let Some(value) = std::env::var_os("HOME") {
    if !value.is_empty() {
      return Some(PathBuf::from(value));
    }
  }

  if let Some(value) = std::env::var_os("USERPROFILE") {
    if !value.is_empty() {
      return Some(PathBuf::from(value));
    }
  }

  match (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
    (Some(drive), Some(path)) if !drive.is_empty() && !path.is_empty() => {
      let mut root = PathBuf::from(drive);
      root.push(path);
      Some(root)
    }
    _ => None,
  }
}

fn config_file_path() -> Result<PathBuf, String> {
  let mut home = home_dir().ok_or_else(|| "无法获取用户主目录".to_string())?;
  home.push(".rustreader");
  home.push("config");
  Ok(home)
}

fn recent_file_path() -> Result<PathBuf, String> {
  let mut home = home_dir().ok_or_else(|| "无法获取用户主目录".to_string())?;
  home.push(".rustreader");
  home.push("recent");
  Ok(home)
}

fn sanitize_recent_entry(value: &str) -> Option<String> {
  let value = value.trim();
  if value.is_empty() {
    return None;
  }
  let value = value.replace('\n', "").replace('\r', "").trim().to_string();
  if value.is_empty() {
    return None;
  }
  Some(value)
}

fn load_recent_from_disk() -> Result<Vec<String>, String> {
  let path = recent_file_path()?;
  let content = match std::fs::read_to_string(&path) {
    Ok(content) => content,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(error) => return Err(format!("读取最近记录失败 ({}): {}", path.display(), error)),
  };

  let mut entries = Vec::new();
  for line in content.lines() {
    let Some(entry) = sanitize_recent_entry(line) else {
      continue;
    };
    if entries.iter().any(|existing| existing == &entry) {
      continue;
    }
    entries.push(entry);
  }

  Ok(entries)
}

fn save_recent_to_disk(entries: &[String]) -> Result<(), String> {
  let path = recent_file_path()?;
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)
      .map_err(|error| format!("创建最近记录目录失败 ({}): {}", parent.display(), error))?;
  }

  let content = if entries.is_empty() {
    String::new()
  } else {
    let mut value = entries.join("\n");
    value.push('\n');
    value
  };

  let tmp_path = path.with_extension("tmp");
  std::fs::write(&tmp_path, content.as_bytes())
    .map_err(|error| format!("写入最近记录失败 ({}): {}", tmp_path.display(), error))?;

  if std::fs::rename(&tmp_path, &path).is_err() {
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&tmp_path, &path)
      .map_err(|error| format!("替换最近记录失败 ({}): {}", path.display(), error))?;
  }

  Ok(())
}

fn record_recent_path(path: &Path) -> Result<(), String> {
  let raw = path.to_string_lossy();
  let Some(value) = sanitize_recent_entry(raw.as_ref()) else {
    return Ok(());
  };

  let mut entries = load_recent_from_disk().unwrap_or_default();
  entries.retain(|existing| existing != &value);
  entries.insert(0, value);
  entries.truncate(RECENT_LIMIT_DEFAULT);
  save_recent_to_disk(&entries)
}

fn strip_app_title_prefix(value: &str) -> &str {
  let raw = value.trim();
  if raw.len() >= APP_TITLE_PREFIX.len() && raw[..APP_TITLE_PREFIX.len()].eq_ignore_ascii_case(APP_TITLE_PREFIX) {
    return raw[APP_TITLE_PREFIX.len()..].trim();
  }
  raw
}

fn build_window_title(site_name: &str) -> String {
  let site_name = strip_app_title_prefix(site_name);
  if site_name.is_empty() {
    return APP_PREFIX.to_string();
  }
  format!("{APP_PREFIX} - {site_name}")
}

fn load_config_from_disk() -> Result<AppConfig, String> {
  let path = config_file_path()?;
  let content = match std::fs::read_to_string(&path) {
    Ok(content) => content,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      return Ok(AppConfig::default());
    }
    Err(error) => {
      return Err(format!("读取配置失败 ({}): {}", path.display(), error));
    }
  };

  if content.trim().is_empty() {
    return Ok(AppConfig::default());
  }

  serde_json::from_str(&content)
    .map_err(|error| format!("解析配置失败 ({}): {}", path.display(), error))
}

fn save_config_to_disk(config: &AppConfig) -> Result<(), String> {
  let path = config_file_path()?;
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)
      .map_err(|error| format!("创建配置目录失败 ({}): {}", parent.display(), error))?;
  }

  let content = serde_json::to_string_pretty(config)
    .map_err(|error| format!("序列化配置失败: {}", error))?;

  let tmp_path = path.with_extension("tmp");
  std::fs::write(&tmp_path, content.as_bytes())
    .map_err(|error| format!("写入配置失败 ({}): {}", tmp_path.display(), error))?;

  if std::fs::rename(&tmp_path, &path).is_err() {
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&tmp_path, &path)
      .map_err(|error| format!("替换配置失败 ({}): {}", path.display(), error))?;
  }

  Ok(())
}

fn parse_cli_open_target(args: impl IntoIterator<Item = OsString>) -> Option<String> {
  let mut iter = args.into_iter().peekable();
  while let Some(arg) = iter.next() {
    let arg_str = arg.to_string_lossy();
    let arg_str = arg_str.trim();
    if arg_str.is_empty() {
      continue;
    }

    if arg_str == "--" {
      if let Some(value) = iter.next() {
        let value = value.to_string_lossy();
        let value = value.trim();
        if !value.is_empty() {
          return Some(value.to_string());
        }
      }
      continue;
    }

    if let Some(_) = arg_str.strip_prefix("--site-name=") {
      continue;
    }

    if arg_str == "--site-name" {
      iter.next();
      continue;
    }

    if let Some(value) = arg_str.strip_prefix("--open=") {
      let value = value.trim();
      if !value.is_empty() {
        return Some(value.to_string());
      }
      continue;
    }

    if let Some(value) = arg_str.strip_prefix("--path=") {
      let value = value.trim();
      if !value.is_empty() {
        return Some(value.to_string());
      }
      continue;
    }

    if arg_str == "--open" || arg_str == "-o" || arg_str == "--path" {
      if let Some(value) = iter.next() {
        let value = value.to_string_lossy();
        let value = value.trim();
        if !value.is_empty() {
          return Some(value.to_string());
        }
      }
      continue;
    }

    if arg_str.starts_with("-psn_") {
      continue;
    }

    if arg_str.starts_with('-') {
      continue;
    }

    return Some(arg_str.to_string());
  }
  None
}

fn parse_cli_site_name(args: impl IntoIterator<Item = OsString>) -> Option<String> {
  let mut iter = args.into_iter();
  while let Some(arg) = iter.next() {
    let arg_str = arg.to_string_lossy();
    let arg_str = arg_str.trim();
    if arg_str.is_empty() {
      continue;
    }

    if arg_str == "--" {
      break;
    }

    if let Some(value) = arg_str.strip_prefix("--site-name=") {
      let value = value.trim();
      if !value.is_empty() {
        return Some(value.to_string());
      }
      continue;
    }

    if arg_str == "--site-name" {
      if let Some(value) = iter.next() {
        let value = value.to_string_lossy();
        let value = value.trim();
        if !value.is_empty() {
          return Some(value.to_string());
        }
      }
      continue;
    }
  }
  None
}

fn categorize_file(path: &Path) -> Option<&'static str> {
  let name_lower = path.file_name()?.to_string_lossy().to_lowercase();
  if name_lower.ends_with(".mm.md") {
    return Some("mindmap");
  }
  if name_lower.ends_with(".ppt.md") {
    return Some("marpit");
  }

  let ext = path.extension()?.to_string_lossy().to_lowercase();
  match ext.as_str() {
    "png" | "jpg" | "jpeg" | "gif" | "webp" => Some("images"),
    "mp4" | "webm" | "ogv" | "m4v" => Some("video"),
    "mp3" | "wav" | "m4a" | "ogg" | "oga" | "flac" | "aac" => Some("audio"),
    "md" | "markdown" => Some("markdown"),
    "drawio" => Some("drawio"),
    "pdf" => Some("pdf"),
    "docx" => Some("word"),
    "xlsx" => Some("excel"),
    "txt" => Some("text"),
    "pptx" => Some("slides"),
    _ => None,
  }
}

fn emit_scan_progress(app: &tauri::AppHandle, payload: ScanProgressEvent) {
  let _ = app.emit(SCAN_PROGRESS_EVENT, payload);
}

fn scan_supported_files(
  app: &tauri::AppHandle,
  scan_id: Option<&str>,
  root: &Path,
) -> Vec<ScanFile> {
  let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
  let mut files = Vec::new();
  let scan_id_owned = scan_id.map(str::to_string);
  let mut scanned_dirs: u64 = 0;
  let mut scanned_files: u64 = 0;
  let mut matched_files: u64 = 0;
  let mut last_emit = Instant::now();
  let emit_interval = Duration::from_millis(120);

  emit_scan_progress(
    app,
    ScanProgressEvent {
      scan_id: scan_id_owned.clone(),
      stage: "start",
      scanned_dirs,
      scanned_files,
      matched_files,
      current_path: root.to_string_lossy().into_owned(),
    },
  );

  while let Some(dir) = stack.pop() {
    scanned_dirs = scanned_dirs.saturating_add(1);
    if last_emit.elapsed() >= emit_interval {
      emit_scan_progress(
        app,
        ScanProgressEvent {
          scan_id: scan_id_owned.clone(),
          stage: "progress",
          scanned_dirs,
          scanned_files,
          matched_files,
          current_path: dir.to_string_lossy().into_owned(),
        },
      );
      last_emit = Instant::now();
    }
    let entries = match std::fs::read_dir(&dir) {
      Ok(entries) => entries,
      Err(_) => continue,
    };

    for entry in entries {
      let entry = match entry {
        Ok(entry) => entry,
        Err(_) => continue,
      };

      let file_type = match entry.file_type() {
        Ok(file_type) => file_type,
        Err(_) => continue,
      };

      let path = entry.path();
      if file_type.is_dir() {
        if last_emit.elapsed() >= emit_interval {
          emit_scan_progress(
            app,
            ScanProgressEvent {
              scan_id: scan_id_owned.clone(),
              stage: "progress",
              scanned_dirs,
              scanned_files,
              matched_files,
              current_path: path.to_string_lossy().into_owned(),
            },
          );
          last_emit = Instant::now();
        }
        stack.push(path);
        continue;
      }
      if !file_type.is_file() {
        continue;
      }

      scanned_files = scanned_files.saturating_add(1);
      let Some(category) = categorize_file(&path) else {
        if last_emit.elapsed() >= emit_interval {
          emit_scan_progress(
            app,
            ScanProgressEvent {
              scan_id: scan_id_owned.clone(),
              stage: "progress",
              scanned_dirs,
              scanned_files,
              matched_files,
              current_path: path.to_string_lossy().into_owned(),
            },
          );
          last_emit = Instant::now();
        }
        continue;
      };
      matched_files = matched_files.saturating_add(1);

      let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => continue,
      };

      let abs_path = path.to_string_lossy().into_owned();
      files.push(ScanFile {
        virtual_path: rel.to_string_lossy().replace('\\', "/"),
        abs_path: abs_path.clone(),
        category: category.to_string(),
      });

      if last_emit.elapsed() >= emit_interval {
        emit_scan_progress(
          app,
          ScanProgressEvent {
            scan_id: scan_id_owned.clone(),
            stage: "progress",
            scanned_dirs,
            scanned_files,
            matched_files,
            current_path: abs_path,
          },
        );
        last_emit = Instant::now();
      }
    }
  }

  emit_scan_progress(
    app,
    ScanProgressEvent {
      scan_id: scan_id_owned,
      stage: "done",
      scanned_dirs,
      scanned_files,
      matched_files,
      current_path: root.to_string_lossy().into_owned(),
    },
  );

  files.sort_by(|a, b| a.virtual_path.cmp(&b.virtual_path));
  files
}

fn normalize_file_url_to_path(raw: &str) -> Cow<'_, str> {
  let value = raw.trim();
  let Some(without_scheme) = value.strip_prefix("file://") else {
    return Cow::Borrowed(value);
  };

  let without_host = without_scheme.strip_prefix("localhost/").unwrap_or(without_scheme);

  if let Some(without_root_slash) = without_host.strip_prefix('/') {
    let bytes = without_root_slash.as_bytes();
    let looks_like_windows_drive =
      bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if looks_like_windows_drive {
      return Cow::Owned(without_root_slash.to_string());
    }
  }

  Cow::Owned(without_host.to_string())
}

#[tauri::command]
fn get_cli_open_target() -> Option<String> {
  parse_cli_open_target(std::env::args_os().skip(1))
}

#[tauri::command]
fn get_cli_site_name() -> Option<String> {
  parse_cli_site_name(std::env::args_os().skip(1))
}

#[tauri::command]
fn set_app_window_title(app: tauri::AppHandle, site_name: String) -> Result<(), String> {
  let title = build_window_title(&site_name);
  for window in app.webview_windows().values() {
    let _ = window.set_title(&title);
  }
  Ok(())
}

#[tauri::command]
fn scan_path(
  app: tauri::AppHandle,
  path: String,
  scan_id: Option<String>,
) -> Result<Option<ScanResult>, String> {
  let raw = path.trim();
  if raw.is_empty() {
    return Ok(None);
  }

  let raw = normalize_file_url_to_path(raw);
  let input_path = PathBuf::from(raw.as_ref());
  let abs_path = input_path
    .canonicalize()
    .map_err(|error| format!("路径不存在或无法访问: {}", error))?;

  if abs_path.is_dir() {
    let _ = record_recent_path(&abs_path);
    let label = abs_path
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_else(|| abs_path.display().to_string());

    return Ok(Some(ScanResult {
      root: abs_path.to_string_lossy().into_owned(),
      label,
      files: scan_supported_files(&app, scan_id.as_deref(), &abs_path),
    }));
  }

  if abs_path.is_file() {
    let Some(category) = categorize_file(&abs_path) else {
      return Err("不支持打开该文件类型（仅支持可预览的文件扩展名）".to_string());
    };
    let _ = record_recent_path(&abs_path);

    let virtual_path = abs_path
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_else(|| abs_path.display().to_string());

    return Ok(Some(ScanResult {
      root: abs_path.to_string_lossy().into_owned(),
      label: virtual_path.clone(),
      files: vec![ScanFile {
        virtual_path,
        abs_path: abs_path.to_string_lossy().into_owned(),
        category: category.to_string(),
      }],
    }));
  }

  Err("路径不是文件或文件夹".to_string())
}

#[tauri::command]
fn pick_and_scan_folder(
  app: tauri::AppHandle,
  scan_id: Option<String>,
) -> Result<Option<ScanResult>, String> {
  let Some(root) = rfd::FileDialog::new().pick_folder() else {
    return Ok(None);
  };
  if !root.is_dir() {
    return Err("选择的路径不是文件夹".to_string());
  }

  let abs_root = root.canonicalize().unwrap_or(root);
  let _ = record_recent_path(&abs_root);

  let label = abs_root
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_else(|| abs_root.display().to_string());

  Ok(Some(ScanResult {
    root: abs_root.to_string_lossy().into_owned(),
    label,
    files: scan_supported_files(&app, scan_id.as_deref(), &abs_root),
  }))
}

#[tauri::command]
fn pick_and_scan_file(
  app: tauri::AppHandle,
  scan_id: Option<String>,
) -> Result<Option<ScanResult>, String> {
  let Some(input) = rfd::FileDialog::new().pick_file() else {
    return Ok(None);
  };

  let abs_path = input.canonicalize().unwrap_or(input);
  if abs_path.is_dir() {
    let _ = record_recent_path(&abs_path);
    let label = abs_path
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_else(|| abs_path.display().to_string());

    return Ok(Some(ScanResult {
      root: abs_path.to_string_lossy().into_owned(),
      label,
      files: scan_supported_files(&app, scan_id.as_deref(), &abs_path),
    }));
  }

  if abs_path.is_file() {
    let Some(category) = categorize_file(&abs_path) else {
      return Err("不支持打开该文件类型（仅支持可预览的文件扩展名）".to_string());
    };
    let _ = record_recent_path(&abs_path);

    let virtual_path = abs_path
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_else(|| abs_path.display().to_string());

    return Ok(Some(ScanResult {
      root: abs_path.to_string_lossy().into_owned(),
      label: virtual_path.clone(),
      files: vec![ScanFile {
        virtual_path,
        abs_path: abs_path.to_string_lossy().into_owned(),
        category: category.to_string(),
      }],
    }));
  }

  Err("路径不是文件或文件夹".to_string())
}

#[tauri::command]
fn load_app_config() -> Result<AppConfig, String> {
  load_config_from_disk()
}

#[tauri::command]
fn save_app_config(config: AppConfig) -> Result<(), String> {
  let mut merged = load_config_from_disk().unwrap_or_default();
  if config.language.is_some() {
    merged.language = config.language;
  }
  if config.font_size_px.is_some() {
    merged.font_size_px = config.font_size_px;
  }
  save_config_to_disk(&merged)
}

#[tauri::command]
fn get_recent_paths(limit: Option<u32>) -> Result<Vec<String>, String> {
  let limit = limit
    .and_then(|value| usize::try_from(value).ok())
    .filter(|value| *value > 0)
    .unwrap_or(RECENT_LIMIT_DEFAULT);

  let mut entries = load_recent_from_disk().unwrap_or_default();
  entries.truncate(limit);
  Ok(entries)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .invoke_handler(tauri::generate_handler![
      get_cli_open_target,
      get_cli_site_name,
      set_app_window_title,
      load_app_config,
      save_app_config,
      get_recent_paths,
      scan_path,
      pick_and_scan_file,
      pick_and_scan_folder
    ])
    .setup(|app| {
      if let Some(site_name) = parse_cli_site_name(std::env::args_os().skip(1)) {
        let site_name = site_name.trim();
        if !site_name.is_empty() {
          let title = build_window_title(site_name);
          for window in app.webview_windows().values() {
            let _ = window.set_title(&title);
          }
        }
      }

      for window in app.webview_windows().values() {
        let _ = window.maximize();
      }

      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      }
      Ok(())
    })
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}

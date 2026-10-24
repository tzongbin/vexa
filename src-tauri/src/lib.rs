use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use fs2::FileExt;
use futures_util::StreamExt;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE},
    multipart::{Form, Part},
    Method, Url,
};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State, WindowEvent};
use tokio_util::sync::CancellationToken;

const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
const MAX_UPLOAD_FILE_BYTES: u64 = 50 * 1024 * 1024;
const SETTINGS_FILE_NAME: &str = "settings.json";
const HISTORY_FILE_NAME: &str = "history.json";
const SESSION_FILE_NAME: &str = "session.json";
const SECRET_KEY_FILE_NAME: &str = "secret.key";
const SECRET_KEY_MAGIC: &[u8; 8] = b"VEXAKEY1";
const ENCRYPTION_ALGORITHM: &str = "AES-256-GCM";
const ENCRYPTION_VERSION: u32 = 1;
const NONCE_BYTES: usize = 12;
const KEY_BYTES: usize = 32;
const JSON_STORE_VERSION: u32 = 1;
const MAX_HISTORY_ENTRIES: usize = 10_000;
const MAX_JSON_STORE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PERSISTED_REQUEST_BYTES: usize = 1024 * 1024;
const HISTORY_LIMITS: [usize; 5] = [100, 500, 1_000, 5_000, 10_000];
const DEFAULT_REQUEST_WIDTH_PERCENT: f64 = 50.0;
const MIN_REQUEST_WIDTH_PERCENT: f64 = 15.0;
const MAX_REQUEST_WIDTH_PERCENT: f64 = 85.0;
const MIN_WINDOW_WIDTH: f64 = 900.0;
const MIN_WINDOW_HEIGHT: f64 = 620.0;
const MAX_WINDOW_DIMENSION: f64 = 16_384.0;
const MAX_WINDOW_COORDINATE: i32 = 2_000_000;
const WINDOW_SAVE_DEBOUNCE: Duration = Duration::from_millis(350);

fn default_request_width_percent() -> f64 {
    DEFAULT_REQUEST_WIDTH_PERCENT
}

fn deserialize_request_width_percent<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?
        .as_f64()
        .unwrap_or(DEFAULT_REQUEST_WIDTH_PERCENT);
    Ok(if valid_request_width_percent(value) {
        value
    } else {
        DEFAULT_REQUEST_WIDTH_PERCENT
    })
}

fn valid_request_width_percent(value: f64) -> bool {
    value.is_finite() && (MIN_REQUEST_WIDTH_PERCENT..=MAX_REQUEST_WIDTH_PERCENT).contains(&value)
}

fn checked_request_width_percent(value: f64) -> Result<f64, String> {
    if valid_request_width_percent(value) {
        Ok((value * 10.0).round() / 10.0)
    } else {
        Err("Session 分割比例必須介於 15% 至 85%。".into())
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum UiTheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum UiFontSize {
    Small,
    Normal,
    #[default]
    Large,
    Xlarge,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppearanceSettings {
    theme: UiTheme,
    font_size: UiFontSize,
    source_font_size: Option<u8>,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: UiTheme::System,
            font_size: UiFontSize::Large,
            source_font_size: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UiSettings {
    version: u32,
    appearance: AppearanceSettings,
    #[serde(default = "default_history_limit")]
    history_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window: Option<WindowSettings>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WindowSettings {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
    scale_factor: f64,
}

impl WindowSettings {
    fn is_valid(&self) -> bool {
        self.x.unsigned_abs() <= MAX_WINDOW_COORDINATE as u32
            && self.y.unsigned_abs() <= MAX_WINDOW_COORDINATE as u32
            && self.scale_factor.is_finite()
            && (0.5..=8.0).contains(&self.scale_factor)
            && (MIN_WINDOW_WIDTH..=MAX_WINDOW_DIMENSION)
                .contains(&(f64::from(self.width) / self.scale_factor))
            && (MIN_WINDOW_HEIGHT..=MAX_WINDOW_DIMENSION)
                .contains(&(f64::from(self.height) / self.scale_factor))
    }
}

const fn default_history_limit() -> usize {
    1_000
}

const fn settings_version() -> u32 {
    1
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            version: settings_version(),
            appearance: AppearanceSettings::default(),
            history_limit: default_history_limit(),
            window: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyUiSettings {
    theme: Option<String>,
    font_size: Option<String>,
    source_font_size: Option<i16>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiSettingsLoadResult {
    settings: UiSettings,
    migrated: bool,
    warning: Option<String>,
}

fn settings_path() -> Result<PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("無法取得執行檔位置：{error}"))?;
    let directory = executable
        .parent()
        .ok_or_else(|| "無法取得執行檔資料夾。".to_owned())?;
    Ok(directory.join("Data").join(SETTINGS_FILE_NAME))
}

fn data_file_path(file_name: &str) -> Result<PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("無法取得執行檔位置：{error}"))?;
    let directory = executable
        .parent()
        .ok_or_else(|| "無法取得執行檔資料夾。".to_owned())?;
    Ok(directory.join("Data").join(file_name))
}

fn read_ui_settings(legacy: Option<LegacyUiSettings>) -> UiSettingsLoadResult {
    let path = match settings_path() {
        Ok(path) => path,
        Err(warning) => {
            return UiSettingsLoadResult {
                settings: UiSettings::default(),
                migrated: false,
                warning: Some(warning),
            }
        }
    };
    read_ui_settings_at(&path, legacy)
}

fn read_ui_settings_at(path: &PathBuf, legacy: Option<LegacyUiSettings>) -> UiSettingsLoadResult {
    if path.exists() {
        match fs::metadata(path) {
            Ok(metadata) if metadata.len() > MAX_JSON_STORE_BYTES as u64 => {
                return UiSettingsLoadResult {
                    settings: UiSettings::default(),
                    migrated: false,
                    warning: Some(
                        "Data/settings.json 超過 32 MiB 上限；已使用預設值，原始檔案已保留。"
                            .into(),
                    ),
                };
            }
            Err(error) => {
                return UiSettingsLoadResult {
                    settings: UiSettings::default(),
                    migrated: false,
                    warning: Some(format!(
                        "無法檢查 Data/settings.json；已使用預設值：{error}"
                    )),
                };
            }
            _ => {}
        }
    }
    match fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str::<UiSettings>(&contents) {
            Ok(mut settings)
                if settings.version == settings_version()
                    && valid_source_size(settings.appearance.source_font_size)
                    && valid_history_limit(settings.history_limit) =>
            {
                if settings
                    .window
                    .as_ref()
                    .is_some_and(|window| !window.is_valid())
                {
                    settings.window = None;
                }
                UiSettingsLoadResult {
                    settings,
                    migrated: false,
                    warning: None,
                }
            }
            Ok(_) => UiSettingsLoadResult {
                settings: UiSettings::default(),
                migrated: false,
                warning: Some("設定檔版本或欄位範圍不支援；已使用預設值，原始檔案已保留。".into()),
            },
            Err(error) => UiSettingsLoadResult {
                settings: UiSettings::default(),
                migrated: false,
                warning: Some(format!(
                    "設定檔格式錯誤；已使用預設值，原始檔案已保留。({error})"
                )),
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let Some(legacy) = legacy else {
                return UiSettingsLoadResult {
                    settings: UiSettings::default(),
                    migrated: false,
                    warning: None,
                };
            };
            let mut settings = UiSettings::default();
            let mut migrated = false;
            if let Some(theme) = legacy.theme.as_deref().and_then(parse_theme) {
                settings.appearance.theme = theme;
                migrated = true;
            }
            if let Some(font_size) = legacy.font_size.as_deref().and_then(parse_font_size) {
                settings.appearance.font_size = font_size;
                migrated = true;
            }
            if let Some(size) = legacy
                .source_font_size
                .filter(|size| (12..=32).contains(size))
            {
                settings.appearance.source_font_size = Some(size as u8);
                migrated = true;
            }
            if !migrated {
                return UiSettingsLoadResult {
                    settings,
                    migrated: false,
                    warning: None,
                };
            }
            match write_ui_settings_file(path, &settings) {
                Ok(()) => UiSettingsLoadResult {
                    settings,
                    migrated: true,
                    warning: None,
                },
                Err(error) => UiSettingsLoadResult {
                    settings,
                    migrated: false,
                    warning: Some(format!(
                        "舊偏好已載入，但無法遷移保存至 Data/settings.json：{error}"
                    )),
                },
            }
        }
        Err(error) => UiSettingsLoadResult {
            settings: UiSettings::default(),
            migrated: false,
            warning: Some(format!(
                "無法讀取 Data/settings.json；已使用預設值：{error}"
            )),
        },
    }
}

fn parse_theme(value: &str) -> Option<UiTheme> {
    match value {
        "system" => Some(UiTheme::System),
        "light" => Some(UiTheme::Light),
        "dark" => Some(UiTheme::Dark),
        _ => None,
    }
}

fn parse_font_size(value: &str) -> Option<UiFontSize> {
    match value {
        "small" => Some(UiFontSize::Small),
        "normal" => Some(UiFontSize::Normal),
        "large" => Some(UiFontSize::Large),
        "xlarge" => Some(UiFontSize::Xlarge),
        _ => None,
    }
}

fn valid_source_size(size: Option<u8>) -> bool {
    size.is_none_or(|value| (12..=32).contains(&value))
}

fn valid_history_limit(limit: usize) -> bool {
    HISTORY_LIMITS.contains(&limit)
}

fn write_ui_settings_file(path: &PathBuf, settings: &UiSettings) -> Result<(), String> {
    if settings.version != settings_version()
        || !valid_source_size(settings.appearance.source_font_size)
        || !valid_history_limit(settings.history_limit)
        || settings
            .window
            .as_ref()
            .is_some_and(|window| !window.is_valid())
    {
        return Err("設定值超出允許範圍。".into());
    }
    let directory = path.parent().ok_or_else(|| "設定資料夾無效。".to_owned())?;
    let mut merged = settings.clone();
    match fs::read_to_string(path) {
        Ok(contents) => {
            let existing = serde_json::from_str::<UiSettings>(&contents)
                .map_err(|_| "現有設定檔格式錯誤，為保留原始檔案，本次不覆寫。".to_owned())?;
            if existing.version != settings_version()
                || !valid_source_size(existing.appearance.source_font_size)
                || !valid_history_limit(existing.history_limit)
            {
                return Err("現有設定檔版本或字級範圍不支援，為保留原始檔案，本次不覆寫。".into());
            }
            if merged.window.is_none() {
                merged.window = existing.window.filter(|window| window.is_valid());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("無法讀取現有設定檔：{error}")),
    }
    write_settings_atomically(path, &merged, directory)
}

fn write_window_settings_file(path: &PathBuf, window: &WindowSettings) -> Result<(), String> {
    if !window.is_valid() {
        return Err("視窗設定值超出允許範圍。".into());
    }
    let directory = path.parent().ok_or_else(|| "設定資料夾無效。".to_owned())?;
    let mut settings = match fs::read_to_string(path) {
        Ok(contents) => {
            let existing = serde_json::from_str::<UiSettings>(&contents)
                .map_err(|_| "現有設定檔格式錯誤，為保留原始檔案，本次不覆寫。".to_owned())?;
            if existing.version != settings_version()
                || !valid_source_size(existing.appearance.source_font_size)
                || !valid_history_limit(existing.history_limit)
            {
                return Err("現有設定檔版本或偏好欄位無效，為保留原始檔案，本次不覆寫。".into());
            }
            existing
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => UiSettings::default(),
        Err(error) => return Err(format!("無法讀取現有設定檔：{error}")),
    };
    settings.window = Some(window.clone());
    write_settings_atomically(path, &settings, directory)
}

fn write_settings_atomically(
    path: &PathBuf,
    settings: &UiSettings,
    directory: &Path,
) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| format!("無法建立 Data 資料夾：{error}"))?;
    let bytes =
        serde_json::to_vec_pretty(settings).map_err(|error| format!("無法編碼設定：{error}"))?;
    if bytes.len() > MAX_JSON_STORE_BYTES {
        return Err("Data/settings.json 超過 32 MiB 上限。".into());
    }
    static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (temporary, mut file) = (0..16)
        .find_map(|_| {
            let sequence = TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let candidate =
                directory.join(format!("settings.{}.{}.tmp", std::process::id(), sequence));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => Some(Ok((candidate, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()
        .map_err(|error| format!("無法建立暫存設定檔：{error}"))?
        .ok_or_else(|| "無法建立唯一的暫存設定檔。".to_owned())?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|error| format!("寫入設定失敗：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("同步設定檔失敗：{error}"))?;
        atomic_replace(&temporary, path).map_err(|error| format!("更新設定檔失敗：{error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Debug, Clone, Copy)]
struct MonitorArea {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale_factor: f64,
    primary: bool,
}

fn resolve_window_settings(
    saved: Option<&WindowSettings>,
    monitors: &[MonitorArea],
) -> Option<WindowSettings> {
    let saved = saved.filter(|settings| settings.is_valid())?;
    if monitors.is_empty() {
        return None;
    }

    let saved_left = i64::from(saved.x);
    let saved_top = i64::from(saved.y);
    let saved_right = saved_left + i64::from(saved.width);
    let saved_bottom = saved_top + i64::from(saved.height);
    let overlap = |monitor: &&MonitorArea| {
        let left = saved_left.max(i64::from(monitor.x));
        let top = saved_top.max(i64::from(monitor.y));
        let right = saved_right.min(i64::from(monitor.x) + i64::from(monitor.width));
        let bottom = saved_bottom.min(i64::from(monitor.y) + i64::from(monitor.height));
        (right - left).max(0) * (bottom - top).max(0)
    };
    let selected = monitors
        .iter()
        .filter(|monitor| {
            monitor.width > 0 && monitor.height > 0 && monitor.scale_factor.is_finite()
        })
        .max_by_key(overlap)
        .filter(|monitor| overlap(monitor) > 0)
        .or_else(|| monitors.iter().find(|monitor| monitor.primary))
        .or_else(|| monitors.first())?;

    let scale_factor = if (0.5..=8.0).contains(&selected.scale_factor) {
        selected.scale_factor
    } else {
        saved.scale_factor
    };
    let scaled = |physical: u32, minimum: f64| {
        ((f64::from(physical) / saved.scale_factor * scale_factor).round())
            .clamp(minimum * scale_factor, MAX_WINDOW_DIMENSION * scale_factor) as u32
    };
    let width = scaled(saved.width, MIN_WINDOW_WIDTH).min(selected.width.max(1));
    let height = scaled(saved.height, MIN_WINDOW_HEIGHT).min(selected.height.max(1));
    let centered = overlap(&selected) == 0;
    let x = if centered {
        i64::from(selected.x) + (i64::from(selected.width) - i64::from(width)).max(0) / 2
    } else {
        i64::from(saved.x).clamp(
            i64::from(selected.x),
            (i64::from(selected.x) + i64::from(selected.width) - i64::from(width))
                .max(i64::from(selected.x)),
        )
    };
    let y = if centered {
        i64::from(selected.y) + (i64::from(selected.height) - i64::from(height)).max(0) / 2
    } else {
        i64::from(saved.y).clamp(
            i64::from(selected.y),
            (i64::from(selected.y) + i64::from(selected.height) - i64::from(height))
                .max(i64::from(selected.y)),
        )
    };

    Some(WindowSettings {
        x: x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y: y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        width,
        height,
        maximized: saved.maximized,
        scale_factor,
    })
}

fn monitor_areas(window: &tauri::WebviewWindow) -> Vec<MonitorArea> {
    let Ok(monitors) = window.available_monitors() else {
        return Vec::new();
    };
    let primary_position = window
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| *monitor.position());
    monitors
        .into_iter()
        .map(|monitor| {
            let work_area = monitor.work_area();
            MonitorArea {
                x: work_area.position.x,
                y: work_area.position.y,
                width: work_area.size.width,
                height: work_area.size.height,
                scale_factor: monitor.scale_factor(),
                primary: primary_position == Some(*monitor.position()),
            }
        })
        .collect()
}

fn geometry_fills_monitor(settings: &WindowSettings, monitors: &[MonitorArea]) -> bool {
    monitors.iter().any(|monitor| {
        (i64::from(settings.x) - i64::from(monitor.x)).abs() <= 32
            && (i64::from(settings.y) - i64::from(monitor.y)).abs() <= 32
            && i64::from(settings.width) >= i64::from(monitor.width) - 48
            && i64::from(settings.height) >= i64::from(monitor.height) - 128
    })
}

fn window_geometry_fills_monitor(window: &tauri::WebviewWindow, settings: &WindowSettings) -> bool {
    geometry_fills_monitor(settings, &monitor_areas(window))
}

fn apply_saved_window(
    window: &tauri::WebviewWindow,
    saved: Option<&WindowSettings>,
) -> Option<WindowSettings> {
    let monitors = monitor_areas(window);
    let restored = resolve_window_settings(saved, &monitors);
    let Some(restored) = restored.as_ref() else {
        let _ = window.center();
        return None;
    };
    let size = tauri::PhysicalSize::new(restored.width, restored.height);
    let position = tauri::PhysicalPosition::new(restored.x, restored.y);
    if window.set_size(size).is_err() || window.set_position(position).is_err() {
        let _ = window.center();
        return None;
    }
    if restored.maximized {
        let _ = window.maximize();
    }
    Some(restored.clone())
}

#[derive(Default)]
struct WindowSaveState {
    latest: Mutex<Option<WindowSettings>>,
    generation: AtomicU64,
}

fn current_window_settings(window: &tauri::WebviewWindow) -> Option<WindowSettings> {
    let position = window.outer_position().ok()?;
    let size = window.inner_size().ok()?;
    let settings = WindowSettings {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized: window.is_maximized().ok()?,
        scale_factor: window.scale_factor().ok()?,
    };
    settings.is_valid().then_some(settings)
}

fn queue_window_settings_save(
    window: tauri::WebviewWindow,
    state: &Arc<WindowSaveState>,
    store: Arc<Mutex<()>>,
) {
    let generation = state.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let state = Arc::clone(state);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(WINDOW_SAVE_DEBOUNCE).await;
        if state.generation.load(Ordering::Acquire) != generation
            || window.is_minimized().unwrap_or(false)
        {
            return;
        }
        let current = if window.is_maximized().unwrap_or(false) {
            let mut current = state.latest.lock().ok().and_then(|value| value.clone());
            if let Some(current) = current.as_mut() {
                current.maximized = true;
            }
            current
        } else {
            current_window_settings(&window)
        };
        let Some(current) = current else {
            return;
        };
        if state.generation.load(Ordering::Acquire) != generation {
            return;
        }
        if let Ok(mut latest) = state.latest.lock() {
            *latest = Some(current.clone());
        }
        let _ = run_store_task(move || {
            let Ok(_guard) = store.lock() else { return };
            if state.generation.load(Ordering::Acquire) != generation {
                return;
            }
            let Ok(path) = settings_path() else { return };
            let Ok(_file_lock) = lock_data_store_for(&path) else {
                return;
            };
            if state.generation.load(Ordering::Acquire) != generation {
                return;
            }
            let _ = write_window_settings_file(&path, &current);
        })
        .await;
    });
}

fn flush_window_settings_save(state: &Arc<WindowSaveState>, store: &Arc<Mutex<()>>) {
    let generation = state.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let Some(current) = state.latest.lock().ok().and_then(|value| value.clone()) else {
        return;
    };
    let Ok(_guard) = store.lock() else { return };
    if state.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let Ok(path) = settings_path() else { return };
    let Ok(_file_lock) = lock_data_store_for(&path) else {
        return;
    };
    if state.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let _ = write_window_settings_file(&path, &current);
}

#[tauri::command]
async fn load_ui_settings(
    legacy: Option<LegacyUiSettings>,
    state: State<'_, JsonStoreWriteState>,
) -> Result<UiSettingsLoadResult, String> {
    let store = Arc::clone(&state.0);
    let result = run_store_task(move || match store.lock() {
        Ok(_guard) => read_ui_settings(legacy),
        Err(_) => UiSettingsLoadResult {
            settings: UiSettings::default(),
            migrated: false,
            warning: Some("JSON 儲存狀態暫時無法使用；已使用預設設定。".into()),
        },
    })
    .await
    .unwrap_or_else(|error| UiSettingsLoadResult {
        settings: UiSettings::default(),
        migrated: false,
        warning: Some(error),
    });
    Ok(result)
}

#[tauri::command]
async fn save_ui_settings(
    settings: UiSettings,
    state: State<'_, JsonStoreWriteState>,
) -> Result<(), String> {
    let store = Arc::clone(&state.0);
    run_store_task(move || {
        let _guard = store
            .lock()
            .map_err(|_| "JSON 儲存狀態暫時無法使用。".to_owned())?;
        let path = settings_path()?;
        let _file_lock = lock_data_store_for(&path)?;
        write_ui_settings_file(&path, &settings)?;
        let history_path = data_file_path(HISTORY_FILE_NAME)?;
        if history_path.exists() {
            let mut history = read_history_at(&history_path);
            if let Some(warning) = history.warning {
                return Err(format!("設定已保存，但無法讀取 History，因此未套用新的上限；原始 History 已保留。{warning}"));
            }
            if history.value.entries.len() > settings.history_limit {
                prune_history(&mut history.value, settings.history_limit);
                write_encrypted_atomically(&history_path, &history.value, "history")?;
            }
        }
        Ok(())
    })
    .await?
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestSnapshotInput {
    method: String,
    url: String,
    timeout_ms: u64,
    params: Vec<KeyValue>,
    headers: Vec<KeyValue>,
    auth: AuthConfig,
    body: RequestBody,
    save_body_content: bool,
    #[serde(default)]
    needs_sensitive_review: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedRequest {
    method: String,
    url: String,
    timeout_ms: u64,
    params: Vec<KeyValue>,
    headers: Vec<KeyValue>,
    auth: AuthConfig,
    body: RequestBody,
    save_body_content: bool,
    needs_sensitive_review: bool,
    body_content_saved: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionTabInput {
    id: String,
    history_id: Option<String>,
    record_history: bool,
    #[serde(default = "default_request_width_percent")]
    request_width_percent: f64,
    request: RequestSnapshotInput,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionTab {
    id: String,
    history_id: Option<String>,
    record_history: bool,
    #[serde(
        default = "default_request_width_percent",
        deserialize_with = "deserialize_request_width_percent"
    )]
    request_width_percent: f64,
    request: PersistedRequest,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryEntry {
    id: String,
    executed_at: String,
    request: PersistedRequest,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HistoryFile {
    version: u32,
    entries: Vec<HistoryEntry>,
}

impl Default for HistoryFile {
    fn default() -> Self {
        Self {
            version: JSON_STORE_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionFile {
    version: u32,
    tabs: Vec<SessionTab>,
    active_tab_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EncryptedJsonFile {
    version: u32,
    algorithm: String,
    nonce: String,
    ciphertext: String,
}

impl Default for SessionFile {
    fn default() -> Self {
        Self {
            version: JSON_STORE_VERSION,
            tabs: Vec::new(),
            active_tab_id: String::new(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonFileLoad<T> {
    value: T,
    warning: Option<String>,
}

struct SecurePayload<T> {
    value: Option<T>,
    warning: Option<String>,
}

#[derive(Default)]
struct JsonStoreWriteState(Arc<Mutex<()>>);

fn lock_data_store_for(path: &Path) -> Result<std::fs::File, String> {
    let directory = path
        .parent()
        .ok_or_else(|| "Data 資料夾無效。".to_owned())?;
    fs::create_dir_all(directory).map_err(|_| "無法建立 Data 資料夾。".to_owned())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(".vexa-store.lock"))
        .map_err(|_| "無法鎖定 Portable Data 儲存區。".to_owned())?;
    lock.lock_exclusive()
        .map_err(|_| "無法鎖定 Portable Data 儲存區。".to_owned())?;
    Ok(lock)
}

async fn run_store_task<T, F>(task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| format!("JSON 儲存背景工作失敗：{error}"))
}

fn validate_request_input(input: &RequestSnapshotInput) -> Result<(), String> {
    if !matches!(
        input.method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) {
        return Err("History/Session 中含有不支援的 HTTP Method。".into());
    }
    if input.url.len() > 32_768 || input.timeout_ms == 0 || input.timeout_ms > 600_000 {
        return Err("History/Session 的 URL 或 Timeout 超出允許範圍。".into());
    }
    if input.params.len() > 2_000 || input.headers.len() > 2_000 || input.body.fields.len() > 2_000
    {
        return Err("History/Session 欄位數量超出允許範圍。".into());
    }
    if !matches!(
        input.auth.kind.as_str(),
        "none" | "basic" | "bearer" | "apiKey"
    ) || !matches!(
        input.body.kind.as_str(),
        "none" | "json" | "text" | "form" | "multipart"
    ) {
        return Err("History/Session 含有不支援的 Auth 或 Body 類型。".into());
    }
    if input.auth.kind == "apiKey" && !matches!(input.auth.location.as_str(), "header" | "query") {
        return Err("History/Session 的 API Key 位置無效。".into());
    }
    if input
        .params
        .iter()
        .chain(input.headers.iter())
        .any(|row| row.key.len() > 8_192 || row.value.len() > 1_048_576)
        || input.body.text.len() > MAX_JSON_STORE_BYTES
        || input.body.fields.iter().any(|field| {
            field.key.len() > 8_192
                || field.value.len() > 1_048_576
                || field.file_path.len() > 32_768
        })
    {
        return Err("History/Session 欄位內容超出允許範圍。".into());
    }
    Ok(())
}

fn valid_persisted_request(request: &PersistedRequest) -> bool {
    validate_request_input(&RequestSnapshotInput {
        method: request.method.clone(),
        url: request.url.clone(),
        timeout_ms: request.timeout_ms,
        params: request.params.clone(),
        headers: request.headers.clone(),
        auth: request.auth.clone(),
        body: request.body.clone(),
        save_body_content: request.save_body_content,
        needs_sensitive_review: request.needs_sensitive_review,
    })
    .is_ok()
}

fn persist_request(input: &RequestSnapshotInput) -> Result<PersistedRequest, String> {
    validate_request_input(input)?;
    let body_content_saved = !input.body.text.is_empty()
        || input
            .body
            .fields
            .iter()
            .any(|field| !field.value.is_empty());
    let request = PersistedRequest {
        method: input.method.clone(),
        url: input.url.clone(),
        timeout_ms: input.timeout_ms,
        params: input.params.clone(),
        headers: input.headers.clone(),
        auth: input.auth.clone(),
        body: input.body.clone(),
        needs_sensitive_review: input.needs_sensitive_review,
        save_body_content: true,
        body_content_saved,
    };
    let size = serde_json::to_vec(&request)
        .map_err(|error| format!("無法檢查 Request 保存大小：{error}"))?
        .len();
    if size > MAX_PERSISTED_REQUEST_BYTES {
        return Err("單筆 Request 超過 1 MiB 保存上限；History/Session 未更新。".into());
    }
    Ok(request)
}

fn validate_history_file(history: &HistoryFile) -> bool {
    history.version == JSON_STORE_VERSION
        && history.entries.len() <= MAX_HISTORY_ENTRIES
        && history.entries.iter().all(|entry| {
            !entry.id.is_empty()
                && entry.executed_at.len() == 24
                && valid_persisted_request(&entry.request)
        })
}

fn validate_session_file(session: &SessionFile) -> bool {
    session.version == JSON_STORE_VERSION
        && !session.tabs.is_empty()
        && session.tabs.iter().all(|tab| {
            !tab.id.is_empty()
                && valid_request_width_percent(tab.request_width_percent)
                && valid_persisted_request(&tab.request)
        })
        && session
            .tabs
            .iter()
            .map(|tab| &tab.id)
            .collect::<std::collections::HashSet<_>>()
            .len()
            == session.tabs.len()
        && session
            .tabs
            .iter()
            .any(|tab| tab.id == session.active_tab_id)
}

fn encryption_aad(kind: &str) -> Vec<u8> {
    format!("Vexa|{kind}|{ENCRYPTION_VERSION}|{ENCRYPTION_ALGORITHM}").into_bytes()
}

fn encode_encrypted_json(
    plaintext: &[u8],
    key: &[u8; KEY_BYTES],
    kind: &str,
) -> Result<Vec<u8>, String> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| "無法初始化 AES-256-GCM。".to_owned())?;
    let mut nonce = [0_u8; NONCE_BYTES];
    getrandom::getrandom(&mut nonce)
        .map_err(|_| "作業系統安全亂數產生失敗；資料未寫入。".to_owned())?;
    let aad = encryption_aad(kind);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| "無法加密 JSON 資料。".to_owned())?;
    let envelope = EncryptedJsonFile {
        version: ENCRYPTION_VERSION,
        algorithm: ENCRYPTION_ALGORITHM.into(),
        nonce: BASE64.encode(nonce),
        ciphertext: BASE64.encode(ciphertext),
    };
    let bytes =
        serde_json::to_vec_pretty(&envelope).map_err(|_| "無法編碼加密 JSON 封裝。".to_owned())?;
    if bytes.len() > MAX_JSON_STORE_BYTES {
        return Err(format!("{kind} 超過 32 MiB 加密儲存上限。"));
    }
    Ok(bytes)
}

fn decode_encrypted_json(
    bytes: &[u8],
    key: &[u8; KEY_BYTES],
    kind: &str,
) -> Result<Vec<u8>, String> {
    let envelope: EncryptedJsonFile = serde_json::from_slice(bytes)
        .map_err(|_| format!("Data/{kind}.json 加密封裝格式錯誤；原始檔案已保留。"))?;
    if envelope.version != ENCRYPTION_VERSION || envelope.algorithm != ENCRYPTION_ALGORITHM {
        return Err(format!(
            "Data/{kind}.json 加密版本或演算法不支援；原始檔案已保留。"
        ));
    }
    let nonce = BASE64
        .decode(envelope.nonce)
        .map_err(|_| format!("Data/{kind}.json Nonce 格式錯誤；原始檔案已保留。"))?;
    if nonce.len() != NONCE_BYTES {
        return Err(format!("Data/{kind}.json Nonce 長度錯誤；原始檔案已保留。"));
    }
    let ciphertext = BASE64
        .decode(envelope.ciphertext)
        .map_err(|_| format!("Data/{kind}.json Ciphertext 格式錯誤；原始檔案已保留。"))?;
    if ciphertext.len() < 16 {
        return Err(format!(
            "Data/{kind}.json Ciphertext 長度錯誤；原始檔案已保留。"
        ));
    }
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| "無法初始化 AES-256-GCM。".to_owned())?;
    let aad = encryption_aad(kind);
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| {
            format!("Data/{kind}.json 認證解密失敗；金鑰錯誤或資料已損壞，原始檔案已保留。")
        })
}

fn read_secret_key(path: &PathBuf) -> Result<[u8; KEY_BYTES], String> {
    let bytes =
        fs::read(path).map_err(|_| "Data/secret.key 無法讀取；加密資料未變更。".to_owned())?;
    if bytes.len() != SECRET_KEY_MAGIC.len() + KEY_BYTES
        || &bytes[..SECRET_KEY_MAGIC.len()] != SECRET_KEY_MAGIC
    {
        return Err("Data/secret.key 格式或長度錯誤；加密資料未變更。".into());
    }
    let mut key = [0_u8; KEY_BYTES];
    key.copy_from_slice(&bytes[SECRET_KEY_MAGIC.len()..]);
    Ok(key)
}

fn protected_file_is_known_legacy(path: &PathBuf) -> Result<bool, String> {
    if !path.exists() {
        return Ok(true);
    }
    let metadata = fs::metadata(path)
        .map_err(|_| "無法檢查既有 History/Session；不建立新金鑰。".to_owned())?;
    if metadata.len() > MAX_JSON_STORE_BYTES as u64 {
        return Ok(false);
    }
    let bytes =
        fs::read(path).map_err(|_| "無法讀取既有 History/Session；不建立新金鑰。".to_owned())?;
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            let raw = String::from_utf8_lossy(&bytes);
            return Ok(!["\"algorithm\"", "\"ciphertext\"", "\"nonce\""]
                .iter()
                .any(|marker| raw.contains(marker)));
        }
    };
    if value.get("algorithm").is_some()
        || value.get("ciphertext").is_some()
        || value.get("nonce").is_some()
    {
        return Ok(false);
    }
    // A v0.2.0 plaintext shape is distinguishable from the encrypted envelope.
    // It is safe to initialize a key for a sibling legacy file even if that file
    // later fails schema validation; it remains untouched and can be repaired.
    let legacy_shape = value.get("version").and_then(serde_json::Value::as_u64)
        == Some(JSON_STORE_VERSION as u64)
        && (value.get("entries").is_some() || value.get("tabs").is_some());
    Ok(legacy_shape)
}

fn get_or_create_secret_key(
    data_dir: &PathBuf,
    allowed_legacy: Option<&PathBuf>,
) -> Result<[u8; KEY_BYTES], String> {
    fs::create_dir_all(data_dir)
        .map_err(|_| "無法建立 Data 資料夾；加密資料未寫入。".to_owned())?;
    let key_path = data_dir.join(SECRET_KEY_FILE_NAME);
    let lock_path = data_dir.join(".secret-key.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|_| "無法鎖定 Data/secret.key 初始化；資料未寫入。".to_owned())?;
    lock.lock_exclusive()
        .map_err(|_| "無法鎖定 Data/secret.key 初始化；資料未寫入。".to_owned())?;
    if key_path.exists() {
        return read_secret_key(&key_path);
    }
    for name in [HISTORY_FILE_NAME, SESSION_FILE_NAME] {
        let protected = data_dir.join(name);
        if allowed_legacy.is_some_and(|allowed| allowed == &protected) || !protected.exists() {
            continue;
        }
        if !protected_file_is_known_legacy(&protected)? {
            return Err("Data/secret.key 遺失或不存在，但已有加密或無法驗證的 History/Session；為避免失去解密能力，不建立新金鑰。".into());
        }
    }
    let mut key = [0_u8; KEY_BYTES];
    getrandom::getrandom(&mut key)
        .map_err(|_| "作業系統安全亂數產生失敗；資料未寫入。".to_owned())?;
    let mut contents = Vec::with_capacity(SECRET_KEY_MAGIC.len() + KEY_BYTES);
    contents.extend_from_slice(SECRET_KEY_MAGIC);
    contents.extend_from_slice(&key);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&key_path)
        .map_err(|_| "無法安全建立 Data/secret.key；既有資料未變更。".to_owned())?;
    if file
        .write_all(&contents)
        .and_then(|_| file.sync_all())
        .is_err()
    {
        let _ = fs::remove_file(&key_path);
        return Err("無法完整保存 Data/secret.key；既有資料未變更。".into());
    }
    Ok(key)
}

fn atomic_replace(temporary: &PathBuf, path: &PathBuf) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let source: Vec<u16> = OsStr::new(temporary).encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();
        let succeeded = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if succeeded == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(temporary, path)
    }
}

fn write_bytes_atomically(path: &PathBuf, bytes: &[u8], label: &str) -> Result<(), String> {
    if bytes.len() > MAX_JSON_STORE_BYTES {
        return Err(format!("{label} 超過 32 MiB 儲存上限。"));
    }
    let directory = path
        .parent()
        .ok_or_else(|| format!("{label} 資料夾無效。"))?;
    fs::create_dir_all(directory).map_err(|_| format!("無法建立 {label} 資料夾。"))?;
    static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (temporary, mut file) = (0..16)
        .find_map(|_| {
            let sequence = TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let candidate =
                directory.join(format!("{label}.{}.{}.tmp", std::process::id(), sequence));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => Some(Ok((candidate, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()
        .map_err(|_| format!("無法建立 {label} 暫存檔。"))?
        .ok_or_else(|| format!("無法建立唯一的 {label} 暫存檔。"))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|_| format!("寫入 {label} 暫存檔失敗。"))?;
        file.sync_all()
            .map_err(|_| format!("同步 {label} 暫存檔失敗。"))?;
        atomic_replace(&temporary, path)
            .map_err(|_| format!("原子替換 {label} 失敗；原始檔案仍保留。"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_encrypted_atomically<T: Serialize>(
    path: &PathBuf,
    value: &T,
    kind: &str,
) -> Result<(), String> {
    let data_dir = path
        .parent()
        .ok_or_else(|| format!("{kind} 資料夾無效。"))?
        .to_path_buf();
    let key = get_or_create_secret_key(&data_dir, None)?;
    let plaintext = serde_json::to_vec(value).map_err(|_| format!("無法編碼 {kind}。"))?;
    if plaintext.len() > MAX_JSON_STORE_BYTES {
        return Err(format!("{kind} 超過 32 MiB 儲存上限。"));
    }
    let bytes = encode_encrypted_json(&plaintext, &key, kind)?;
    write_bytes_atomically(path, &bytes, kind)
}

fn cleanup_legacy_migration_artifacts(path: &Path, kind: &str) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("{kind} 資料夾無效。"))?;
    let entries = fs::read_dir(directory)
        .map_err(|_| format!("Data/{kind}.json 已加密，但無法檢查舊版明文暫存檔。"))?;
    for entry in entries {
        let entry =
            entry.map_err(|_| format!("Data/{kind}.json 已加密，但無法檢查舊版明文暫存檔。"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let fixed_backup = [
            format!("{kind}.bak"),
            format!("{kind}.old"),
            format!("{kind}.json.bak"),
            format!("{kind}.json.old"),
        ]
        .contains(&name);
        let legacy_temp = name
            .strip_prefix(&format!("{kind}."))
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .is_some_and(|parts| {
                let mut pieces = parts.split('.');
                pieces
                    .next()
                    .is_some_and(|part| part.parse::<u32>().is_ok())
                    && pieces
                        .next()
                        .is_some_and(|part| part.parse::<u64>().is_ok())
                    && pieces.next().is_none()
            });
        if fixed_backup || legacy_temp {
            let file_type = entry
                .file_type()
                .map_err(|_| format!("Data/{kind}.json 已加密，但無法檢查舊版明文暫存檔。"))?;
            if file_type.is_file() || file_type.is_symlink() {
                fs::remove_file(entry.path())
                    .map_err(|_| format!("Data/{kind}.json 已加密，但舊版明文暫存檔無法清除。"))?;
            }
        }
    }
    Ok(())
}

fn read_secure_payload<T>(
    path: &PathBuf,
    kind: &str,
    validate: fn(&T) -> bool,
) -> Result<SecurePayload<T>, String>
where
    T: for<'de> Deserialize<'de> + Serialize + PartialEq,
{
    if !path.exists() {
        return Ok(SecurePayload {
            value: None,
            warning: None,
        });
    }
    let metadata =
        fs::metadata(path).map_err(|_| format!("無法檢查 Data/{kind}.json；原始檔案已保留。"))?;
    if metadata.len() > MAX_JSON_STORE_BYTES as u64 {
        return Err(format!(
            "Data/{kind}.json 超過 32 MiB 上限；原始檔案已保留。"
        ));
    }
    let bytes =
        fs::read(path).map_err(|_| format!("無法讀取 Data/{kind}.json；原始檔案已保留。"))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("Data/{kind}.json 格式錯誤；原始檔案已保留。"))?;
    let encrypted = value.get("algorithm").is_some()
        || value.get("ciphertext").is_some()
        || value.get("nonce").is_some();
    if encrypted {
        let data_dir = path
            .parent()
            .ok_or_else(|| format!("{kind} 資料夾無效。"))?;
        let key_path = data_dir.join(SECRET_KEY_FILE_NAME);
        if !key_path.exists() {
            return Err(format!(
                "Data/{kind}.json 已加密，但 Data/secret.key 遺失；檔案未覆蓋。"
            ));
        }
        let key = read_secret_key(&key_path)?;
        let plaintext = decode_encrypted_json(&bytes, &key, kind)?;
        let loaded: T = serde_json::from_slice(&plaintext)
            .map_err(|_| format!("Data/{kind}.json 解密後資料格式錯誤；原始檔案已保留。"))?;
        if !validate(&loaded) {
            return Err(format!(
                "Data/{kind}.json 版本或資料欄位無效；原始檔案已保留。"
            ));
        }
        let warning = cleanup_legacy_migration_artifacts(path, kind).err();
        return Ok(SecurePayload {
            value: Some(loaded),
            warning,
        });
    }
    let legacy: T = serde_json::from_slice(&bytes)
        .map_err(|_| format!("Data/{kind}.json 舊版格式錯誤；原始檔案已保留。"))?;
    if !validate(&legacy) {
        return Err(format!(
            "Data/{kind}.json 舊版版本或資料欄位無效；原始檔案已保留。"
        ));
    }
    let data_dir = path
        .parent()
        .ok_or_else(|| format!("{kind} 資料夾無效。"))?
        .to_path_buf();
    let key = get_or_create_secret_key(&data_dir, Some(path))?;
    let sealed = encode_encrypted_json(&bytes, &key, kind)?;
    let round_trip = decode_encrypted_json(&sealed, &key, kind)?;
    let verified: T = serde_json::from_slice(&round_trip)
        .map_err(|_| format!("Data/{kind}.json 遷移驗證失敗；原始檔案保留。"))?;
    if verified != legacy || !validate(&verified) {
        return Err(format!("Data/{kind}.json 遷移內容驗證失敗；原始檔案保留。"));
    }
    write_bytes_atomically(path, &sealed, kind)?;
    let warning = cleanup_legacy_migration_artifacts(path, kind).err();
    Ok(SecurePayload {
        value: Some(verified),
        warning,
    })
}

fn read_history_at(path: &PathBuf) -> JsonFileLoad<HistoryFile> {
    match read_secure_payload(path, "history", validate_history_file) {
        Ok(SecurePayload {
            value: Some(value),
            warning,
        }) => JsonFileLoad { value, warning },
        Ok(SecurePayload {
            value: None,
            warning,
        }) => JsonFileLoad {
            value: HistoryFile::default(),
            warning,
        },
        Err(error) => JsonFileLoad {
            value: HistoryFile::default(),
            warning: Some(error),
        },
    }
}

fn read_session_at(path: &PathBuf) -> JsonFileLoad<SessionFile> {
    match read_secure_payload(path, "session", validate_session_file) {
        Ok(SecurePayload {
            value: Some(value),
            warning,
        }) => JsonFileLoad { value, warning },
        Ok(SecurePayload {
            value: None,
            warning,
        }) => JsonFileLoad {
            value: SessionFile::default(),
            warning,
        },
        Err(error) => JsonFileLoad {
            value: SessionFile::default(),
            warning: Some(error),
        },
    }
}

fn prune_history(history: &mut HistoryFile, limit: usize) {
    history
        .entries
        .sort_by(|a, b| b.executed_at.cmp(&a.executed_at));
    history.entries.truncate(limit);
}

#[tauri::command]
async fn load_history(
    state: State<'_, JsonStoreWriteState>,
) -> Result<JsonFileLoad<HistoryFile>, String> {
    let store = Arc::clone(&state.0);
    let result = run_store_task(move || {
        let _guard = match store.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return JsonFileLoad {
                    value: HistoryFile::default(),
                    warning: Some("JSON 儲存狀態暫時無法使用。".into()),
                }
            }
        };
        match data_file_path(HISTORY_FILE_NAME) {
            Ok(path) => match lock_data_store_for(&path) {
                Ok(_file_lock) => read_history_at(&path),
                Err(error) => JsonFileLoad {
                    value: HistoryFile::default(),
                    warning: Some(error),
                },
            },
            Err(error) => JsonFileLoad {
                value: HistoryFile::default(),
                warning: Some(error),
            },
        }
    })
    .await
    .unwrap_or_else(|error| JsonFileLoad {
        value: HistoryFile::default(),
        warning: Some(error),
    });
    Ok(result)
}

#[tauri::command]
async fn record_history(
    input: RequestSnapshotInput,
    state: State<'_, JsonStoreWriteState>,
) -> Result<(), String> {
    let store = Arc::clone(&state.0);
    run_store_task(move || {
        let _guard = store
            .lock()
            .map_err(|_| "JSON 儲存狀態暫時無法使用。".to_owned())?;
        let settings_path = data_file_path(SETTINGS_FILE_NAME)?;
        let settings = read_ui_settings_at(&settings_path, None).settings;
        let path = data_file_path(HISTORY_FILE_NAME)?;
        let _file_lock = lock_data_store_for(&path)?;
        let mut loaded = read_history_at(&path);
        if let Some(warning) = loaded.warning.as_deref().filter(|_| path.exists()) {
            return Err(format!("History 未寫入，原始檔案已保留。{warning}"));
        }
        let request = persist_request(&input)?;
        let id = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        loaded.value.entries.insert(
            0,
            HistoryEntry {
                id,
                executed_at: chrono_compatible_timestamp(),
                request,
            },
        );
        prune_history(&mut loaded.value, settings.history_limit);
        write_encrypted_atomically(&path, &loaded.value, "history")
    })
    .await?
}

fn chrono_compatible_timestamp() -> String {
    // UTC epoch milliseconds are locale-independent and sort lexicographically.
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!(
        "{}.{:03}Z",
        unix_seconds_to_iso(millis / 1_000),
        millis % 1_000
    )
}

fn unix_seconds_to_iso(seconds: u128) -> String {
    // Convert UTC days using the Gregorian calendar without adding a date dependency.
    let days = (seconds / 86_400) as i64;
    let day_seconds = (seconds % 86_400) as u64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        day_seconds / 3_600,
        (day_seconds % 3_600) / 60,
        day_seconds % 60
    )
}

#[tauri::command]
async fn clear_history(
    scope: String,
    today_date: Option<String>,
    today_start_utc: Option<String>,
    tomorrow_start_utc: Option<String>,
    state: State<'_, JsonStoreWriteState>,
) -> Result<(), String> {
    let store = Arc::clone(&state.0);
    run_store_task(move || {
        let _guard = store
            .lock()
            .map_err(|_| "JSON 儲存狀態暫時無法使用。".to_owned())?;
        let path = data_file_path(HISTORY_FILE_NAME)?;
        let _file_lock = lock_data_store_for(&path)?;
        let mut loaded = read_history_at(&path);
        if let Some(warning) = loaded.warning.as_deref().filter(|_| path.exists()) {
            return Err(format!("History 未清除，原始檔案已保留。{warning}"));
        }
        if scope == "all" {
            loaded.value.entries.clear();
        } else if let Some(days) = match scope.as_str() {
            "today" => Some(0),
            "older7" => Some(7),
            "older30" => Some(30),
            _ => None,
        } {
            let cutoff = std::time::SystemTime::now() - Duration::from_secs(days * 86_400);
            let cutoff_millis = cutoff
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let cutoff_iso = unix_seconds_to_iso(cutoff_millis / 1_000);
            if days == 0 {
                if let (Some(start), Some(end)) = (today_start_utc, tomorrow_start_utc) {
                    if start.len() != 24 || end.len() != 24 {
                        return Err("History 今天的日期範圍無效。".into());
                    }
                    loaded
                        .value
                        .entries
                        .retain(|entry| entry.executed_at < start || entry.executed_at >= end);
                } else {
                    let today = today_date
                        .filter(|date| {
                            date.len() == 10
                                && date.as_bytes().get(4) == Some(&b'-')
                                && date.as_bytes().get(7) == Some(&b'-')
                        })
                        .unwrap_or_else(|| cutoff_iso[..10].to_owned());
                    loaded
                        .value
                        .entries
                        .retain(|entry| entry.executed_at.get(..10) != Some(today.as_str()));
                }
            } else {
                loaded
                    .value
                    .entries
                    .retain(|entry| entry.executed_at < cutoff_iso);
            }
        } else {
            return Err("History 清除範圍無效。".into());
        }
        write_encrypted_atomically(&path, &loaded.value, "history")
    })
    .await?
}

#[tauri::command]
async fn load_session(
    state: State<'_, JsonStoreWriteState>,
) -> Result<JsonFileLoad<SessionFile>, String> {
    let store = Arc::clone(&state.0);
    let result = run_store_task(move || {
        let _guard = match store.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return JsonFileLoad {
                    value: SessionFile::default(),
                    warning: Some("JSON 儲存狀態暫時無法使用。".into()),
                }
            }
        };
        match data_file_path(SESSION_FILE_NAME) {
            Ok(path) => match lock_data_store_for(&path) {
                Ok(_file_lock) => read_session_at(&path),
                Err(error) => JsonFileLoad {
                    value: SessionFile::default(),
                    warning: Some(error),
                },
            },
            Err(error) => JsonFileLoad {
                value: SessionFile::default(),
                warning: Some(error),
            },
        }
    })
    .await
    .unwrap_or_else(|error| JsonFileLoad {
        value: SessionFile::default(),
        warning: Some(error),
    });
    Ok(result)
}

#[tauri::command]
async fn save_session(
    tabs: Vec<SessionTabInput>,
    active_tab_id: String,
    state: State<'_, JsonStoreWriteState>,
) -> Result<(), String> {
    let store = Arc::clone(&state.0);
    run_store_task(move || {
        let _guard = store
            .lock()
            .map_err(|_| "JSON 儲存狀態暫時無法使用。".to_owned())?;
        if tabs.is_empty() {
            return Err("Session 至少需要一個 Tab。".into());
        }
        let mut seen = std::collections::HashSet::new();
        let mut saved_tabs = Vec::with_capacity(tabs.len());
        for tab in tabs {
            if tab.id.is_empty() || tab.id.len() > 128 || !seen.insert(tab.id.clone()) {
                return Err("Session Tab ID 無效或重複。".into());
            }
            saved_tabs.push(SessionTab {
                id: tab.id,
                history_id: tab.history_id,
                record_history: tab.record_history,
                request_width_percent: checked_request_width_percent(tab.request_width_percent)?,
                request: persist_request(&tab.request)?,
            });
        }
        let active = if seen.contains(&active_tab_id) {
            active_tab_id
        } else {
            saved_tabs[0].id.clone()
        };
        let session = SessionFile {
            version: JSON_STORE_VERSION,
            tabs: saved_tabs,
            active_tab_id: active,
        };
        let path = data_file_path(SESSION_FILE_NAME)?;
        let _file_lock = lock_data_store_for(&path)?;
        if path.exists() {
            if let Some(warning) = read_session_at(&path).warning {
                return Err(format!("Session 未寫入，原始檔案已保留。{warning}"));
            }
        }
        write_encrypted_atomically(&path, &session, "session")
    })
    .await?
}

#[derive(Default)]
struct EngineState {
    active: Mutex<HashMap<String, CancellationToken>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpRequest {
    tab_id: String,
    execution_id: String,
    method: String,
    url: String,
    timeout_ms: u64,
    params: Vec<KeyValue>,
    headers: Vec<KeyValue>,
    auth: AuthConfig,
    body: RequestBody,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct KeyValue {
    key: String,
    value: String,
    enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct AuthConfig {
    kind: String,
    username: String,
    password: String,
    token: String,
    key: String,
    value: String,
    location: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RequestBody {
    kind: String,
    text: String,
    fields: Vec<BodyField>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct BodyField {
    key: String,
    value: String,
    enabled: bool,
    is_file: bool,
    file_path: String,
    content_type: String,
}

#[tauri::command]
async fn validate_multipart_files(body: RequestBody) -> Result<Vec<String>, String> {
    run_store_task(move || {
        if body.kind != "multipart" {
            return Vec::new();
        }
        body.fields
            .iter()
            .filter(|field| field.enabled && field.is_file)
            .filter_map(|field| {
                if field.file_path.is_empty() {
                    return Some(format!(
                        "{}：尚未選取檔案",
                        if field.key.is_empty() {
                            "檔案欄位"
                        } else {
                            &field.key
                        }
                    ));
                }
                let path = PathBuf::from(&field.file_path);
                match fs::metadata(&path) {
                    Ok(metadata)
                        if metadata.is_file() && metadata.len() <= MAX_UPLOAD_FILE_BYTES =>
                    {
                        None
                    }
                    _ => Some(format!(
                        "{}：{}",
                        if field.key.is_empty() {
                            "檔案欄位"
                        } else {
                            &field.key
                        },
                        field.file_path
                    )),
                }
            })
            .collect()
    })
    .await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HttpResult {
    tab_id: String,
    execution_id: String,
    response: Option<HttpResponse>,
    error: Option<HttpError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HttpResponse {
    status: u16,
    status_text: String,
    duration_ms: u128,
    response_size: u64,
    truncated: bool,
    headers: Vec<ResponseHeader>,
    body_base64: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseHeader {
    name: String,
    value: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HttpError {
    kind: String,
    message: String,
}

fn failure(
    tab_id: String,
    execution_id: String,
    kind: &str,
    message: impl Into<String>,
) -> HttpResult {
    HttpResult {
        tab_id,
        execution_id,
        response: None,
        error: Some(HttpError {
            kind: kind.to_owned(),
            message: message.into(),
        }),
    }
}

fn build_url(raw: &str, params: &[KeyValue], auth: &AuthConfig) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL 為必填欄位。".to_owned());
    }
    let mut url = Url::parse(trimmed)
        .map_err(|_| "URL 格式錯誤，請檢查網址並使用完整的 HTTP 或 HTTPS 網址。".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("僅支援包含主機名稱的 HTTP 或 HTTPS 網址。".to_owned());
    }
    let has_extra_params = params
        .iter()
        .any(|pair| pair.enabled && !pair.key.is_empty());
    let has_api_key_query =
        auth.kind == "apiKey" && auth.location == "query" && !auth.key.is_empty();
    if has_extra_params || has_api_key_query {
        let mut query = url.query_pairs_mut();
        for pair in params
            .iter()
            .filter(|pair| pair.enabled && !pair.key.is_empty())
        {
            query.append_pair(&pair.key, &pair.value);
        }
        if auth.kind == "apiKey" && auth.location == "query" && !auth.key.is_empty() {
            query.append_pair(&auth.key, &auth.value);
        }
    }
    Ok(url)
}

fn append_header(headers: &mut HeaderMap, name: &str, value: &str) -> Result<(), String> {
    let name = HeaderName::from_bytes(name.trim().as_bytes())
        .map_err(|_| format!("無效的 Header 名稱：{}", name.trim()))?;
    let value =
        HeaderValue::from_str(value).map_err(|_| format!("Header「{}」包含不合法字元。", name))?;
    headers.append(name, value);
    Ok(())
}

enum OutgoingBody {
    Bytes(Vec<u8>),
    Multipart(Form),
}

async fn build_body(
    request: &HttpRequest,
    headers: &mut HeaderMap,
) -> Result<Option<OutgoingBody>, String> {
    let custom_content_type = headers.contains_key(CONTENT_TYPE);
    match request.body.kind.as_str() {
        "none" => Ok(None),
        "json" | "text" => {
            if !custom_content_type {
                let content_type = if request.body.kind == "json" {
                    "application/json"
                } else {
                    "text/plain; charset=utf-8"
                };
                headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
            }
            Ok(Some(OutgoingBody::Bytes(
                request.body.text.as_bytes().to_vec(),
            )))
        }
        "form" => {
            let fields: Vec<(String, String)> = request
                .body
                .fields
                .iter()
                .filter(|field| field.enabled && !field.is_file && !field.key.is_empty())
                .map(|field| (field.key.clone(), field.value.clone()))
                .collect();
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in fields {
                serializer.append_pair(&key, &value);
            }
            let encoded = serializer.finish().into_bytes();
            if !custom_content_type {
                headers.insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                );
            }
            Ok(Some(OutgoingBody::Bytes(encoded)))
        }
        "multipart" => {
            // reqwest generates the boundary; a manually supplied Content-Type would be incomplete.
            headers.remove(CONTENT_TYPE);
            let mut form = Form::new();
            for field in request
                .body
                .fields
                .iter()
                .filter(|field| field.enabled && !field.key.is_empty())
            {
                if field.is_file {
                    let path = PathBuf::from(&field.file_path);
                    if !path.is_absolute() {
                        return Err("上傳檔案路徑無效，請重新選取檔案。".to_owned());
                    }
                    let metadata = tokio::fs::metadata(&path)
                        .await
                        .map_err(|error| format!("無法讀取上傳檔案資訊：{error}"))?;
                    if !metadata.is_file() {
                        return Err("選取的上傳項目不是檔案。".to_owned());
                    }
                    if metadata.len() > MAX_UPLOAD_FILE_BYTES {
                        return Err("單一上傳檔案上限為 50 MB。".to_owned());
                    }
                    let bytes = tokio::fs::read(&path)
                        .await
                        .map_err(|error| format!("無法讀取上傳檔案：{error}"))?;
                    let file_name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("upload.bin")
                        .to_owned();
                    let mut part = Part::bytes(bytes).file_name(file_name);
                    if !field.content_type.trim().is_empty() {
                        part = part.mime_str(field.content_type.trim()).map_err(|_| {
                            format!("檔案欄位「{}」的 Content-Type 無效。", field.key)
                        })?;
                    }
                    form = form.part(field.key.clone(), part);
                } else {
                    form = form.text(field.key.clone(), field.value.clone());
                }
            }
            Ok(Some(OutgoingBody::Multipart(form)))
        }
        _ => Err("不支援的 Request Body 類型。".to_owned()),
    }
}

async fn execute_http(
    request: &HttpRequest,
    cancellation: CancellationToken,
) -> Result<HttpResponse, HttpError> {
    if !(1..=600_000).contains(&request.timeout_ms) {
        return Err(HttpError {
            kind: "invalid_request".into(),
            message: "Timeout 必須介於 1 毫秒與 10 分鐘。".into(),
        });
    }
    let method = Method::from_bytes(request.method.trim().to_ascii_uppercase().as_bytes())
        .map_err(|_| HttpError {
            kind: "invalid_request".into(),
            message: "HTTP Method 無效。".into(),
        })?;
    if !matches!(
        method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) {
        return Err(HttpError {
            kind: "invalid_request".into(),
            message: "不支援的 HTTP Method。".into(),
        });
    }
    let url =
        build_url(&request.url, &request.params, &request.auth).map_err(|message| HttpError {
            kind: "invalid_request".into(),
            message,
        })?;

    let mut headers = HeaderMap::new();
    for pair in request
        .headers
        .iter()
        .filter(|pair| pair.enabled && !pair.key.trim().is_empty())
    {
        append_header(&mut headers, &pair.key, &pair.value).map_err(|message| HttpError {
            kind: "invalid_request".into(),
            message,
        })?;
    }
    let has_user_authorization = headers.contains_key("authorization");
    let body = build_body(request, &mut headers)
        .await
        .map_err(|message| HttpError {
            kind: "invalid_request".into(),
            message,
        })?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(request.timeout_ms))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|error| HttpError {
            kind: "network".into(),
            message: format!("無法建立 HTTP Client：{error}"),
        })?;

    let mut builder = client.request(method, url).headers(headers);
    match request.auth.kind.as_str() {
        "basic" if !has_user_authorization => {
            builder = builder.basic_auth(&request.auth.username, Some(&request.auth.password))
        }
        "bearer" if !has_user_authorization && !request.auth.token.is_empty() => {
            builder = builder.bearer_auth(&request.auth.token)
        }
        "apiKey" if request.auth.location == "header" && !request.auth.key.is_empty() => {
            builder = builder.header(&request.auth.key, &request.auth.value);
        }
        _ => {}
    }
    if let Some(body) = body {
        builder = match body {
            OutgoingBody::Bytes(bytes) => builder.body(bytes),
            OutgoingBody::Multipart(form) => builder.multipart(form),
        };
    }

    let started = std::time::Instant::now();
    let response = tokio::select! {
        _ = cancellation.cancelled() => return Err(HttpError { kind: "canceled".into(), message: "Request 已取消。".into() }),
        result = builder.send() => result.map_err(|error| {
            let timed_out = error.is_timeout();
            let kind = if timed_out { "timeout" } else if error.is_builder() { "invalid_request" } else { "network" };
            let safe_error = error.without_url();
            HttpError { kind: kind.into(), message: if timed_out { "Request 逾時，請檢查網址或調整 Timeout。".into() } else { format!("HTTP 連線失敗：{safe_error}") } }
        })?,
    };

    let status = response.status();
    let status_text = status
        .canonical_reason()
        .unwrap_or("Unknown Status")
        .to_owned();
    let response_headers: Vec<ResponseHeader> = response
        .headers()
        .iter()
        .map(|(name, value)| ResponseHeader {
            name: name.as_str().to_owned(),
            value: value
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|_| String::from_utf8_lossy(value.as_bytes()).into_owned()),
        })
        .collect();
    let announced_size = response.content_length();
    let mut stream = response.bytes_stream();
    let mut body = Vec::with_capacity(MAX_RESPONSE_BYTES.min(64 * 1024));
    let mut received = 0u64;
    let mut truncated = announced_size.is_some_and(|size| size > MAX_RESPONSE_BYTES as u64);

    loop {
        let next = tokio::select! {
            _ = cancellation.cancelled() => return Err(HttpError { kind: "canceled".into(), message: "Request 已取消。".into() }),
            next = stream.next() => next,
        };
        let Some(chunk_result) = next else { break };
        let chunk = chunk_result.map_err(|error| {
            let timed_out = error.is_timeout();
            HttpError {
                kind: if timed_out { "timeout" } else { "network" }.into(),
                message: if timed_out {
                    "讀取 Response 逾時。".into()
                } else {
                    format!("讀取 Response 失敗：{}", error.without_url())
                },
            }
        })?;
        received = received.saturating_add(chunk.len() as u64);
        let remaining = MAX_RESPONSE_BYTES.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if chunk.len() > remaining {
            truncated = true;
            break;
        }
    }
    if received > MAX_RESPONSE_BYTES as u64 {
        truncated = true;
    }
    let response_size = announced_size.unwrap_or(received).max(body.len() as u64);

    Ok(HttpResponse {
        status: status.as_u16(),
        status_text,
        duration_ms: started.elapsed().as_millis(),
        response_size,
        truncated,
        headers: response_headers,
        body_base64: BASE64.encode(body),
    })
}

#[tauri::command]
async fn send_request(
    request: HttpRequest,
    state: State<'_, EngineState>,
) -> Result<HttpResult, String> {
    if request.tab_id.is_empty() || request.execution_id.is_empty() {
        return Ok(failure(
            request.tab_id,
            request.execution_id,
            "invalid_request",
            "Request 識別資訊無效。",
        ));
    }
    let cancellation = CancellationToken::new();
    {
        let Ok(mut active) = state.active.lock() else {
            return Ok(failure(
                request.tab_id,
                request.execution_id,
                "internal",
                "HTTP Engine 狀態暫時無法使用。",
            ));
        };
        if active.contains_key(&request.execution_id) {
            return Ok(failure(
                request.tab_id,
                request.execution_id,
                "invalid_request",
                "此 Request 已在執行中。",
            ));
        }
        active.insert(request.execution_id.clone(), cancellation.clone());
    }

    let result = match execute_http(&request, cancellation).await {
        Ok(response) => HttpResult {
            tab_id: request.tab_id.clone(),
            execution_id: request.execution_id.clone(),
            response: Some(response),
            error: None,
        },
        Err(error) => HttpResult {
            tab_id: request.tab_id.clone(),
            execution_id: request.execution_id.clone(),
            response: None,
            error: Some(error),
        },
    };
    if let Ok(mut active) = state.active.lock() {
        active.remove(&request.execution_id);
    }
    Ok(result)
}

#[tauri::command]
fn cancel_request(execution_id: String, state: State<'_, EngineState>) -> bool {
    state
        .active
        .lock()
        .ok()
        .and_then(|active| active.get(&execution_id).cloned())
        .map(|token| token.cancel())
        .is_some()
}

#[tauri::command]
async fn save_response(path: String, body_base64: String) -> Result<(), String> {
    let bytes = BASE64
        .decode(body_base64)
        .map_err(|_| "Response 資料格式無效。".to_owned())?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err("Response 儲存大小超過 5 MiB。".to_owned());
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("儲存路徑無效，請重新選擇目的檔案。".to_owned());
    }
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| format!("無法儲存 Response：{error}"))
}

pub fn run() {
    tauri::Builder::default()
        .manage(EngineState::default())
        .manage(JsonStoreWriteState::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let store = Arc::clone(&app.state::<JsonStoreWriteState>().0);
            let saved = read_ui_settings(None).settings.window;
            if let Some(window) = app.get_webview_window("main") {
                let restored = apply_saved_window(&window, saved.as_ref());
                let save_state = Arc::new(WindowSaveState::default());
                if let Ok(mut latest) = save_state.latest.lock() {
                    *latest = restored.or_else(|| current_window_settings(&window));
                }

                let event_window = window.clone();
                let event_state = Arc::clone(&save_state);
                let event_store = Arc::clone(&store);
                window.on_window_event(move |event| match event {
                    WindowEvent::Moved(_)
                    | WindowEvent::Resized(_)
                    | WindowEvent::ScaleFactorChanged { .. } => {
                        if !event_window.is_minimized().unwrap_or(false) {
                            if event_window.is_maximized().unwrap_or(false) {
                                if let Ok(mut latest) = event_state.latest.lock() {
                                    if let Some(current) = latest.as_mut() {
                                        current.maximized = true;
                                    }
                                }
                            } else if let Some(mut current) = current_window_settings(&event_window)
                            {
                                if !window_geometry_fills_monitor(&event_window, &current) {
                                    current.maximized = false;
                                    if let Ok(mut latest) = event_state.latest.lock() {
                                        *latest = Some(current);
                                    }
                                }
                            }
                        }
                        queue_window_settings_save(
                            event_window.clone(),
                            &event_state,
                            Arc::clone(&event_store),
                        );
                    }
                    WindowEvent::CloseRequested { .. } => {
                        if !event_window.is_minimized().unwrap_or(false) {
                            if event_window.is_maximized().unwrap_or(false) {
                                if let Ok(mut latest) = event_state.latest.lock() {
                                    if let Some(current) = latest.as_mut() {
                                        current.maximized = true;
                                    }
                                }
                            } else if let Some(current) = current_window_settings(&event_window) {
                                if let Ok(mut latest) = event_state.latest.lock() {
                                    *latest = Some(current);
                                }
                            }
                        }
                        flush_window_settings_save(&event_state, &event_store);
                    }
                    _ => {}
                });
                window.show()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            send_request,
            validate_multipart_files,
            cancel_request,
            save_response,
            load_ui_settings,
            save_ui_settings,
            load_history,
            record_history,
            clear_history,
            load_session,
            save_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vexa");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc, Mutex,
        },
        thread::{self, JoinHandle},
    };

    struct TempDirectory(PathBuf);
    impl TempDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "vexa-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn settings_use_validated_defaults_and_portable_json_persistence() {
        let directory = TempDirectory::new("settings");
        let path = directory.0.join("Data").join(SETTINGS_FILE_NAME);
        let missing = read_ui_settings_at(&path, None);
        assert_eq!(missing.settings, UiSettings::default());
        assert!(missing.warning.is_none());
        assert!(
            !path.exists(),
            "default load must not create the settings file"
        );

        let migrated = read_ui_settings_at(
            &path,
            Some(LegacyUiSettings {
                theme: Some("dark".into()),
                font_size: Some("normal".into()),
                source_font_size: Some(24),
            }),
        );
        assert!(migrated.migrated);
        assert_eq!(migrated.settings.appearance.theme, UiTheme::Dark);
        assert_eq!(migrated.settings.appearance.font_size, UiFontSize::Normal);
        assert_eq!(migrated.settings.appearance.source_font_size, Some(24));
        assert!(path.exists());

        let json = std::fs::read_to_string(&path).unwrap();
        let loaded = read_ui_settings_at(
            &path,
            Some(LegacyUiSettings {
                theme: Some("light".into()),
                font_size: Some("xlarge".into()),
                source_font_size: Some(32),
            }),
        );
        assert!(
            !loaded.migrated,
            "existing JSON must take precedence over legacy values"
        );
        assert_eq!(loaded.settings, migrated.settings);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), json);

        let changed = UiSettings {
            version: 1,
            history_limit: 5_000,
            appearance: AppearanceSettings {
                theme: UiTheme::Light,
                font_size: UiFontSize::Xlarge,
                source_font_size: None,
            },
            window: None,
        };
        write_ui_settings_file(&path, &changed).unwrap();
        assert_eq!(read_ui_settings_at(&path, None).settings, changed);
    }

    fn window_settings(
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        maximized: bool,
        scale: f64,
    ) -> WindowSettings {
        WindowSettings {
            x,
            y,
            width,
            height,
            maximized,
            scale_factor: scale,
        }
    }

    #[test]
    fn window_settings_merge_without_overwriting_existing_preferences() {
        let directory = TempDirectory::new("window-settings-merge");
        let path = directory.0.join("Data").join(SETTINGS_FILE_NAME);
        let first = window_settings(-1200, 80, 1600, 1000, false, 1.0);
        write_window_settings_file(&path, &first).unwrap();

        let appearance = UiSettings {
            version: 1,
            appearance: AppearanceSettings {
                theme: UiTheme::Dark,
                font_size: UiFontSize::Small,
                source_font_size: Some(18),
            },
            history_limit: 5_000,
            window: None,
        };
        write_ui_settings_file(&path, &appearance).unwrap();
        let loaded = read_ui_settings_at(&path, None).settings;
        assert_eq!(loaded.appearance, appearance.appearance);
        assert_eq!(loaded.history_limit, 5_000);
        assert_eq!(loaded.window, Some(first.clone()));

        let second = window_settings(-1400, 100, 1800, 1100, true, 1.25);
        write_window_settings_file(&path, &second).unwrap();
        let loaded = read_ui_settings_at(&path, None).settings;
        assert_eq!(loaded.appearance, appearance.appearance);
        assert_eq!(loaded.history_limit, 5_000);
        assert_eq!(loaded.window, Some(second));
    }

    #[test]
    fn window_restore_keeps_negative_monitor_coordinates_and_scales_for_dpi() {
        let negative_monitor = MonitorArea {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
            primary: false,
        };
        let primary = MonitorArea {
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
            scale_factor: 1.5,
            primary: true,
        };
        let negative = window_settings(-1700, 100, 1600, 1000, false, 1.0);
        let restored =
            resolve_window_settings(Some(&negative), &[negative_monitor, primary]).unwrap();
        assert_eq!((restored.x, restored.y), (-1700, 80));
        assert_eq!((restored.width, restored.height), (1600, 1000));

        let on_primary = window_settings(100, 100, 2400, 1500, false, 2.0);
        let restored =
            resolve_window_settings(Some(&on_primary), &[negative_monitor, primary]).unwrap();
        assert_eq!((restored.width, restored.height), (1800, 1125));
        assert_eq!(restored.scale_factor, 1.5);
    }

    #[test]
    fn missing_disconnected_or_invalid_window_settings_fall_back_to_visible_geometry() {
        let primary = MonitorArea {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
            primary: true,
        };
        assert!(resolve_window_settings(None, &[primary]).is_none());
        assert!(resolve_window_settings(
            Some(&window_settings(0, 0, 800, 600, false, 1.0)),
            &[primary]
        )
        .is_none());
        assert!(resolve_window_settings(
            Some(&window_settings(i32::MIN, 0, 1440, 900, false, 1.0)),
            &[primary]
        )
        .is_none());
        assert!(resolve_window_settings(
            Some(&window_settings(0, 0, 1440, 900, false, f64::INFINITY)),
            &[primary]
        )
        .is_none());

        let unplugged = window_settings(3000, 2000, 1200, 800, false, 1.0);
        let restored = resolve_window_settings(Some(&unplugged), &[primary]).unwrap();
        assert_eq!((restored.x, restored.y), (360, 140));
        assert!(restored.x >= primary.x && restored.y >= primary.y);
        assert!(restored.x + restored.width as i32 <= primary.width as i32);
        assert!(restored.y + restored.height as i32 <= primary.height as i32);

        let maximized_bounds = window_settings(0, 0, 1900, 1000, false, 1.0);
        assert!(geometry_fills_monitor(&maximized_bounds, &[primary]));
        let normal_bounds = window_settings(180, 120, 1320, 800, false, 1.0);
        assert!(!geometry_fills_monitor(&normal_bounds, &[primary]));
    }

    #[test]
    fn invalid_window_geometry_is_ignored_without_losing_valid_settings() {
        let directory = TempDirectory::new("window-settings-validation");
        let path = directory.0.join(SETTINGS_FILE_NAME);
        let json = r#"{"version":1,"appearance":{"theme":"dark","fontSize":"small","sourceFontSize":18},"historyLimit":5000,"window":{"x":999999999,"y":-120,"width":1600,"height":900,"maximized":false,"scaleFactor":1.0}}"#;
        std::fs::write(&path, json).unwrap();
        let loaded = read_ui_settings_at(&path, None);
        assert_eq!(loaded.settings.appearance.theme, UiTheme::Dark);
        assert_eq!(loaded.settings.appearance.font_size, UiFontSize::Small);
        assert_eq!(loaded.settings.history_limit, 5_000);
        assert_eq!(loaded.settings.window, None);
        assert!(loaded.warning.is_none());
    }

    #[test]
    fn window_save_generations_discard_stale_debounced_writes() {
        let state = WindowSaveState::default();
        let old_generation = state.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let new_generation = state.generation.fetch_add(1, Ordering::AcqRel) + 1;
        assert_ne!(old_generation, new_generation);
        assert_ne!(state.generation.load(Ordering::Acquire), old_generation);
        assert_eq!(state.generation.load(Ordering::Acquire), new_generation);
    }

    #[test]
    fn corrupt_or_unwritable_settings_are_preserved_without_blocking_startup() {
        let directory = TempDirectory::new("bad-settings");
        let data = directory.0.join("Data");
        std::fs::create_dir_all(&data).unwrap();
        let path = data.join(SETTINGS_FILE_NAME);
        let corrupt = b"{not json";
        std::fs::write(&path, corrupt).unwrap();
        let loaded = read_ui_settings_at(
            &path,
            Some(LegacyUiSettings {
                theme: Some("dark".into()),
                font_size: Some("small".into()),
                source_font_size: None,
            }),
        );
        assert_eq!(loaded.settings, UiSettings::default());
        assert!(!loaded.migrated);
        assert!(loaded.warning.is_some());
        let desired = UiSettings::default();
        assert!(write_ui_settings_file(&path, &desired).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);

        let blocked_path = directory.0.join("unreadable-data").join(SETTINGS_FILE_NAME);
        std::fs::create_dir_all(&blocked_path).unwrap();
        let unavailable = read_ui_settings_at(&blocked_path, None);
        assert_eq!(unavailable.settings, UiSettings::default());
        assert!(unavailable.warning.is_some());
        assert!(write_ui_settings_file(&blocked_path, &desired).is_err());
    }

    #[test]
    fn settings_reject_unknown_values_and_out_of_range_source_font_size() {
        let directory = TempDirectory::new("settings-validation");
        let path = directory.0.join(SETTINGS_FILE_NAME);
        for json in [
            r#"{"version":1,"appearance":{"theme":"system","fontSize":"giant","sourceFontSize":null}}"#,
            r#"{"version":1,"appearance":{"theme":"system","fontSize":"large","sourceFontSize":33}}"#,
            r#"{"version":2,"appearance":{"theme":"system","fontSize":"large","sourceFontSize":null}}"#,
            r#"{"appearance":{"theme":"system","fontSize":"large","sourceFontSize":null}}"#,
            r#"{"version":1,"appearance":{"theme":"system","fontSize":"large","extra":true}}"#,
        ] {
            std::fs::write(&path, json).unwrap();
            let loaded = read_ui_settings_at(&path, None);
            assert_eq!(loaded.settings, UiSettings::default());
            assert!(loaded.warning.is_some());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), json);
        }
    }

    fn request_snapshot(save_body_content: bool) -> RequestSnapshotInput {
        RequestSnapshotInput {
            method: "POST".into(),
            url: "https://example.test/api?access_token=url-secret&page=2".into(),
            timeout_ms: 30_000,
            params: vec![KeyValue {
                key: "api_key".into(),
                value: "query-secret".into(),
                enabled: true,
            }],
            headers: vec![
                KeyValue {
                    key: "Authorization".into(),
                    value: "Bearer header-secret".into(),
                    enabled: true,
                },
                KeyValue {
                    key: "Content-Type".into(),
                    value: "application/json".into(),
                    enabled: true,
                },
            ],
            auth: AuthConfig {
                kind: "basic".into(),
                username: "fixture-user".into(),
                password: "basic-secret".into(),
                token: String::new(),
                key: String::new(),
                value: String::new(),
                location: "header".into(),
            },
            body: RequestBody {
                kind: "json".into(),
                text: r#"{"password":"body-secret"}"#.into(),
                fields: Vec::new(),
            },
            save_body_content,
            needs_sensitive_review: false,
        }
    }

    #[test]
    fn session_split_percent_defaults_old_and_invalid_values_and_rejects_invalid_writes() {
        let session = SessionFile {
            version: JSON_STORE_VERSION,
            tabs: vec![SessionTab {
                id: "split-tab".into(),
                history_id: None,
                record_history: true,
                request_width_percent: 65.0,
                request: persist_request(&request_snapshot(true)).unwrap(),
            }],
            active_tab_id: "split-tab".into(),
        };

        let mut old_session = serde_json::to_value(&session).unwrap();
        old_session["tabs"][0]
            .as_object_mut()
            .unwrap()
            .remove("requestWidthPercent");
        let old_session: SessionFile = serde_json::from_value(old_session).unwrap();
        assert_eq!(
            old_session.tabs[0].request_width_percent,
            DEFAULT_REQUEST_WIDTH_PERCENT
        );

        for invalid in [
            serde_json::Value::String("NaN".into()),
            serde_json::Value::Null,
        ] {
            let mut invalid_session = serde_json::to_value(&session).unwrap();
            invalid_session["tabs"][0]["requestWidthPercent"] = invalid;
            let invalid_session: SessionFile = serde_json::from_value(invalid_session).unwrap();
            assert_eq!(
                invalid_session.tabs[0].request_width_percent,
                DEFAULT_REQUEST_WIDTH_PERCENT
            );
        }

        for invalid in [-1.0, 0.0, 14.9, 85.1, 100.0] {
            let mut invalid_session = serde_json::to_value(&session).unwrap();
            invalid_session["tabs"][0]["requestWidthPercent"] = invalid.into();
            let invalid_session: SessionFile = serde_json::from_value(invalid_session).unwrap();
            assert_eq!(
                invalid_session.tabs[0].request_width_percent,
                DEFAULT_REQUEST_WIDTH_PERCENT
            );
        }

        for valid in [15.0, 32.0, 35.0, 50.0, 65.0, 68.0, 85.0] {
            assert_eq!(checked_request_width_percent(valid).unwrap(), valid);
            let mut compatible_session = serde_json::to_value(&session).unwrap();
            compatible_session["tabs"][0]["requestWidthPercent"] = valid.into();
            let compatible_session: SessionFile =
                serde_json::from_value(compatible_session).unwrap();
            assert!(validate_session_file(&compatible_session));
            assert_eq!(compatible_session.tabs[0].request_width_percent, valid);
        }
        assert_eq!(checked_request_width_percent(65.04).unwrap(), 65.0);
        assert_eq!(checked_request_width_percent(65.06).unwrap(), 65.1);
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 14.9, 85.1] {
            assert!(checked_request_width_percent(invalid).is_err());
        }
    }

    #[test]
    fn history_and_session_retain_complete_request_values_for_encryption() {
        let saved = persist_request(&request_snapshot(false)).unwrap();
        assert!(!saved.needs_sensitive_review);
        assert!(saved.body_content_saved);
        assert_eq!(saved.body.text, r#"{"password":"body-secret"}"#);
        assert_eq!(saved.auth.password, "basic-secret");
        assert_eq!(saved.headers[0].value, "Bearer header-secret");
        assert_eq!(saved.params[0].value, "query-secret");
        assert!(saved.url.contains("access_token=url-secret"));
        assert!(saved.save_body_content);
    }

    #[test]
    fn multipart_persists_text_fields_and_external_file_reference() {
        let mut input = request_snapshot(true);
        input.body.kind = "multipart".into();
        input.body.text.clear();
        input.body.fields = vec![
            BodyField {
                key: "password".into(),
                value: "field-secret".into(),
                enabled: true,
                is_file: false,
                file_path: String::new(),
                content_type: String::new(),
            },
            BodyField {
                key: "document".into(),
                value: String::new(),
                enabled: true,
                is_file: true,
                file_path: "C:\\private\\document.pdf".into(),
                content_type: "application/pdf".into(),
            },
        ];
        let saved = persist_request(&input).unwrap();
        assert_eq!(saved.body.fields[0].value, "field-secret");
        assert_eq!(saved.body.fields[1].file_path, "C:\\private\\document.pdf");
        assert_eq!(saved.body.fields[1].content_type, "application/pdf");
        assert!(!saved.needs_sensitive_review);
    }

    #[test]
    fn single_request_persistence_limit_is_enforced_without_truncating_request_content() {
        let mut input = request_snapshot(true);
        input.body.text = "x".repeat(MAX_PERSISTED_REQUEST_BYTES + 1);
        let error = persist_request(&input).unwrap_err();
        assert!(error.contains("1 MiB"));
        assert_eq!(input.body.text.len(), MAX_PERSISTED_REQUEST_BYTES + 1);
    }

    #[test]
    fn plaintext_history_and_session_migrate_and_round_trip_as_ciphertext() {
        let directory = TempDirectory::new("json-stores");
        let history_path = directory.0.join("Data").join(HISTORY_FILE_NAME);
        let session_path = directory.0.join("Data").join(SESSION_FILE_NAME);
        let request = persist_request(&request_snapshot(false)).unwrap();
        let mut history = HistoryFile::default();
        history.entries.push(HistoryEntry {
            id: "one".into(),
            executed_at: "2026-10-24T01:02:03.000Z".into(),
            request: request.clone(),
        });
        std::fs::create_dir_all(history_path.parent().unwrap()).unwrap();
        std::fs::write(&history_path, serde_json::to_vec_pretty(&history).unwrap()).unwrap();
        assert_eq!(
            read_history_at(&history_path).value.entries,
            history.entries
        );
        let encrypted_history = std::fs::read(&history_path).unwrap();
        assert!(String::from_utf8_lossy(&encrypted_history).contains("AES-256-GCM"));
        assert!(!String::from_utf8_lossy(&encrypted_history).contains("basic-secret"));
        assert!(!String::from_utf8_lossy(&encrypted_history).contains("body-secret"));
        assert_eq!(
            read_history_at(&history_path).value.entries,
            history.entries
        );
        assert_eq!(
            std::fs::read(&history_path).unwrap(),
            encrypted_history,
            "already encrypted payload is not migrated again"
        );

        let session = SessionFile {
            version: JSON_STORE_VERSION,
            tabs: vec![SessionTab {
                id: "tab-one".into(),
                history_id: Some("one".into()),
                record_history: true,
                request_width_percent: 50.0,
                request,
            }],
            active_tab_id: "tab-one".into(),
        };
        std::fs::write(&session_path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();
        assert_eq!(read_session_at(&session_path).value.tabs, session.tabs);
        let encrypted_session = std::fs::read(&session_path).unwrap();
        assert!(!String::from_utf8_lossy(&encrypted_session).contains("basic-secret"));
        assert!(!String::from_utf8_lossy(&encrypted_session).contains("body-secret"));
        assert_eq!(read_session_at(&session_path).value.tabs, session.tabs);

        std::fs::write(&history_path, b"{broken").unwrap();
        let corrupt_bytes = std::fs::read(&history_path).unwrap();
        let corrupt = read_history_at(&history_path);
        assert!(corrupt.warning.is_some());
        assert!(corrupt.value.entries.is_empty());
        assert_eq!(corrupt_bytes, std::fs::read(&history_path).unwrap());
    }

    #[test]
    fn aes_gcm_uses_fresh_nonces_and_authenticates_payload_nonce_key_and_format() {
        let key = [0x5a; KEY_BYTES];
        let plaintext = br#"{"token":"fake-token","password":"fake-password"}"#;
        let first = encode_encrypted_json(plaintext, &key, "history").unwrap();
        let second = encode_encrypted_json(plaintext, &key, "history").unwrap();
        let first_envelope: EncryptedJsonFile = serde_json::from_slice(&first).unwrap();
        let second_envelope: EncryptedJsonFile = serde_json::from_slice(&second).unwrap();
        assert_ne!(first_envelope.nonce, second_envelope.nonce);
        assert_eq!(first_envelope.version, ENCRYPTION_VERSION);
        assert_eq!(first_envelope.algorithm, ENCRYPTION_ALGORITHM);
        assert!(!String::from_utf8_lossy(&first).contains("fake-token"));
        assert!(!String::from_utf8_lossy(&first).contains("fake-password"));
        assert_eq!(
            decode_encrypted_json(&first, &key, "history").unwrap(),
            plaintext
        );
        assert!(decode_encrypted_json(&first, &[0x33; KEY_BYTES], "history").is_err());
        assert!(
            decode_encrypted_json(&first, &key, "session").is_err(),
            "store purpose is authenticated in AAD"
        );

        let mut payload_tampered = first_envelope.clone();
        let mut ciphertext = BASE64.decode(&payload_tampered.ciphertext).unwrap();
        ciphertext[0] ^= 1;
        payload_tampered.ciphertext = BASE64.encode(ciphertext);
        assert!(decode_encrypted_json(
            &serde_json::to_vec(&payload_tampered).unwrap(),
            &key,
            "history"
        )
        .is_err());

        let mut nonce_tampered = first_envelope.clone();
        let mut nonce = BASE64.decode(&nonce_tampered.nonce).unwrap();
        nonce[0] ^= 1;
        nonce_tampered.nonce = BASE64.encode(nonce);
        assert!(decode_encrypted_json(
            &serde_json::to_vec(&nonce_tampered).unwrap(),
            &key,
            "history"
        )
        .is_err());

        let mut bad_nonce_length = first_envelope.clone();
        bad_nonce_length.nonce = BASE64.encode([0_u8; 11]);
        assert!(decode_encrypted_json(
            &serde_json::to_vec(&bad_nonce_length).unwrap(),
            &key,
            "history"
        )
        .unwrap_err()
        .contains("Nonce 長度"));
        let mut future_version = first_envelope;
        future_version.version += 1;
        assert!(decode_encrypted_json(
            &serde_json::to_vec(&future_version).unwrap(),
            &key,
            "history"
        )
        .unwrap_err()
        .contains("不支援"));
    }

    #[test]
    fn key_loss_corruption_and_wrong_key_never_replace_encrypted_json() {
        let directory = TempDirectory::new("key-failures");
        let data = directory.0.join("Data");
        let history_path = data.join(HISTORY_FILE_NAME);
        let mut history = HistoryFile::default();
        history.entries.push(HistoryEntry {
            id: "retained".into(),
            executed_at: "2026-10-24T01:02:03.000Z".into(),
            request: persist_request(&request_snapshot(false)).unwrap(),
        });
        write_encrypted_atomically(&history_path, &history, "history").unwrap();
        let encrypted = fs::read(&history_path).unwrap();
        let key_path = data.join(SECRET_KEY_FILE_NAME);

        fs::remove_file(&key_path).unwrap();
        assert!(read_history_at(&history_path)
            .warning
            .unwrap()
            .contains("secret.key 遺失"));
        assert_eq!(fs::read(&history_path).unwrap(), encrypted);
        assert!(
            !key_path.exists(),
            "a lost key is never silently regenerated"
        );
        let session = SessionFile {
            version: JSON_STORE_VERSION,
            tabs: vec![SessionTab {
                id: "new-tab".into(),
                history_id: None,
                record_history: true,
                request_width_percent: 50.0,
                request: history.entries[0].request.clone(),
            }],
            active_tab_id: "new-tab".into(),
        };
        assert!(
            write_encrypted_atomically(&data.join(SESSION_FILE_NAME), &session, "session").is_err()
        );
        assert!(
            !key_path.exists(),
            "saving another store must not replace a lost key"
        );
        assert!(!data.join(SESSION_FILE_NAME).exists());

        fs::write(&key_path, b"bad key").unwrap();
        assert!(read_history_at(&history_path)
            .warning
            .unwrap()
            .contains("格式或長度錯誤"));
        assert_eq!(fs::read(&history_path).unwrap(), encrypted);

        let mut wrong_key = Vec::from(SECRET_KEY_MAGIC.as_slice());
        wrong_key.extend_from_slice(&[0x44; KEY_BYTES]);
        fs::write(&key_path, wrong_key).unwrap();
        assert!(read_history_at(&history_path)
            .warning
            .unwrap()
            .contains("認證解密失敗"));
        assert_eq!(fs::read(&history_path).unwrap(), encrypted);

        let key = read_secret_key(&key_path).unwrap();
        let mut envelope: EncryptedJsonFile = serde_json::from_slice(&encrypted).unwrap();
        let mut ciphertext = BASE64.decode(&envelope.ciphertext).unwrap();
        ciphertext[0] ^= 0x80;
        envelope.ciphertext = BASE64.encode(ciphertext);
        let tampered = serde_json::to_vec(&envelope).unwrap();
        fs::write(&history_path, &tampered).unwrap();
        assert!(read_history_at(&history_path)
            .warning
            .unwrap()
            .contains("認證解密失敗"));
        assert_eq!(fs::read(&history_path).unwrap(), tampered);
        assert!(!encode_encrypted_json(b"x", &key, "history")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn encrypted_stores_are_portable_and_high_frequency_replacements_keep_latest_value() {
        let directory = TempDirectory::new("portable-encrypted");
        let source = directory.0.join("source").join("Data");
        let history_path = source.join(HISTORY_FILE_NAME);
        let mut history = HistoryFile::default();
        history.entries.push(HistoryEntry {
            id: "original".into(),
            executed_at: "2026-10-24T01:02:03.000Z".into(),
            request: persist_request(&request_snapshot(false)).unwrap(),
        });
        write_encrypted_atomically(&history_path, &history, "history").unwrap();
        let session = SessionFile {
            version: JSON_STORE_VERSION,
            tabs: vec![SessionTab {
                id: "tab-portable".into(),
                history_id: None,
                record_history: true,
                request_width_percent: 50.0,
                request: history.entries[0].request.clone(),
            }],
            active_tab_id: "tab-portable".into(),
        };
        let source_session = source.join(SESSION_FILE_NAME);
        write_encrypted_atomically(&source_session, &session, "session").unwrap();

        let destination = directory.0.join("copied-app").join("Data");
        fs::create_dir_all(&destination).unwrap();
        for name in [HISTORY_FILE_NAME, SESSION_FILE_NAME, SECRET_KEY_FILE_NAME] {
            fs::copy(source.join(name), destination.join(name)).unwrap();
        }
        assert_eq!(
            read_history_at(&destination.join(HISTORY_FILE_NAME)).value,
            history
        );
        assert_eq!(
            read_session_at(&destination.join(SESSION_FILE_NAME)).value,
            session
        );

        for index in 0..30 {
            history.entries[0].id = format!("saved-{index}");
            write_encrypted_atomically(&history_path, &history, "history").unwrap();
        }
        assert_eq!(
            read_history_at(&history_path).value.entries[0].id,
            "saved-29"
        );
    }

    #[test]
    fn legacy_masked_fields_are_preserved_as_missing_during_encryption_migration() {
        let directory = TempDirectory::new("legacy-mask");
        let path = directory.0.join("Data").join(HISTORY_FILE_NAME);
        let mut old = persist_request(&request_snapshot(false)).unwrap();
        old.auth.token.clear();
        old.auth.password.clear();
        old.params[0].value.clear();
        old.headers[0].value.clear();
        old.body.text.clear();
        old.needs_sensitive_review = true;
        old.body_content_saved = false;
        let history = HistoryFile {
            version: JSON_STORE_VERSION,
            entries: vec![HistoryEntry {
                id: "old-masked".into(),
                executed_at: "2026-10-24T01:02:03.000Z".into(),
                request: old.clone(),
            }],
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec(&history).unwrap()).unwrap();
        let migrated = read_history_at(&path);
        assert!(migrated.warning.is_none());
        assert_eq!(migrated.value.entries[0].request, old);
        assert!(migrated.value.entries[0].request.needs_sensitive_review);
        assert!(migrated.value.entries[0].request.auth.password.is_empty());
        assert!(migrated.value.entries[0].request.body.text.is_empty());
    }

    #[test]
    fn partial_migration_is_retryable_and_does_not_rotate_the_existing_key() {
        let directory = TempDirectory::new("partial-migration");
        let data = directory.0.join("Data");
        let history_path = data.join(HISTORY_FILE_NAME);
        let session_path = data.join(SESSION_FILE_NAME);
        let request = persist_request(&request_snapshot(false)).unwrap();
        let history = HistoryFile {
            version: JSON_STORE_VERSION,
            entries: vec![HistoryEntry {
                id: "migrated-first".into(),
                executed_at: "2026-10-24T01:02:03.000Z".into(),
                request: request.clone(),
            }],
        };
        fs::create_dir_all(&data).unwrap();
        fs::write(&history_path, serde_json::to_vec(&history).unwrap()).unwrap();
        let history_temp = data.join("history.123.4.tmp");
        let history_backup = data.join("history.json.bak");
        fs::write(&history_temp, b"old plaintext token").unwrap();
        fs::write(&history_backup, b"old plaintext body").unwrap();
        let mut rejected_request = request.clone();
        rejected_request.method = "TRACE".into();
        let rejected_session = SessionFile {
            version: JSON_STORE_VERSION,
            tabs: vec![SessionTab {
                id: "too-large-tab".into(),
                history_id: None,
                record_history: true,
                request_width_percent: 50.0,
                request: rejected_request,
            }],
            active_tab_id: "too-large-tab".into(),
        };
        let rejected_bytes = serde_json::to_vec(&rejected_session).unwrap();
        fs::write(&session_path, &rejected_bytes).unwrap();
        let session_temp = data.join("session.123.4.tmp");
        fs::write(&session_temp, b"old plaintext token").unwrap();

        assert_eq!(read_history_at(&history_path).value, history);
        assert!(!history_temp.exists());
        assert!(!history_backup.exists());
        let key_before = fs::read(data.join(SECRET_KEY_FILE_NAME)).unwrap();
        let encrypted_history = fs::read(&history_path).unwrap();
        assert!(read_session_at(&session_path).warning.is_some());
        assert_eq!(fs::read(&session_path).unwrap(), rejected_bytes);
        assert!(
            session_temp.exists(),
            "failed migration preserves related plaintext artifacts"
        );

        let session = SessionFile {
            version: JSON_STORE_VERSION,
            tabs: vec![SessionTab {
                id: "retry-tab".into(),
                history_id: None,
                record_history: false,
                request_width_percent: 50.0,
                request,
            }],
            active_tab_id: "retry-tab".into(),
        };
        fs::write(&session_path, serde_json::to_vec(&session).unwrap()).unwrap();
        assert_eq!(read_session_at(&session_path).value, session);
        assert!(!session_temp.exists());
        assert_eq!(
            fs::read(data.join(SECRET_KEY_FILE_NAME)).unwrap(),
            key_before
        );
        assert_eq!(fs::read(&history_path).unwrap(), encrypted_history);
    }

    #[test]
    fn secret_key_initialization_is_serialized_across_threads() {
        let directory = TempDirectory::new("key-race");
        let data = directory.0.join("Data");
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let data = data.clone();
                thread::spawn(move || get_or_create_secret_key(&data, None).unwrap())
            })
            .collect();
        let keys: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert!(keys.iter().all(|key| key == &keys[0]));
        assert_eq!(fs::read(data.join(SECRET_KEY_FILE_NAME)).unwrap().len(), 40);
    }

    #[test]
    fn atomic_replace_failure_keeps_existing_destination_bytes() {
        let directory = TempDirectory::new("atomic-replace-failure");
        let destination = directory.0.join("destination.json");
        // A directory cannot be atomically replaced by a file, so this exercises the
        // platform replace error path without risking the existing destination.
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep"), b"keep").unwrap();
        assert!(write_bytes_atomically(&destination, b"replacement bytes", "test").is_err());
        assert_eq!(fs::read(destination.join("keep")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn history_limit_is_validated_and_oldest_entries_are_trimmed() {
        assert!(valid_history_limit(100));
        assert!(valid_history_limit(1_000));
        assert!(!valid_history_limit(999));
        let mut history = HistoryFile::default();
        for (id, executed_at) in [
            ("old", "2026-01-01T00:00:00.000Z"),
            ("new", "2026-10-24T00:00:00.000Z"),
        ] {
            history.entries.push(HistoryEntry {
                id: id.into(),
                executed_at: executed_at.into(),
                request: persist_request(&request_snapshot(false)).unwrap(),
            });
        }
        prune_history(&mut history, 1);
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].id, "new");
    }

    #[derive(Debug)]
    struct CapturedRequest {
        method: String,
        target: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl CapturedRequest {
        fn header_values(&self, name: &str) -> Vec<&str> {
            self.headers
                .iter()
                .filter(|(header, _)| header.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
                .collect()
        }
    }

    struct MockResponse {
        bytes: Vec<u8>,
    }

    impl MockResponse {
        fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
            let body = if status == 204 {
                Vec::new()
            } else {
                body.into()
            };
            let reason = match status {
                200 => "OK",
                201 => "Created",
                204 => "No Content",
                301 => "Moved Permanently",
                302 => "Found",
                400 => "Bad Request",
                401 => "Unauthorized",
                404 => "Not Found",
                429 => "Too Many Requests",
                500 => "Internal Server Error",
                503 => "Service Unavailable",
                _ => "Fixture",
            };
            let mut bytes = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/octet-stream\r\n\r\n",
                body.len()
            )
            .into_bytes();
            bytes.extend_from_slice(&body);
            Self { bytes }
        }

        fn with_headers(mut self, headers: &[(&str, &str)]) -> Self {
            let body_start = self
                .bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("fixture response has a header terminator");
            let body = self.bytes.split_off(body_start + 4);
            self.bytes.truncate(body_start);
            for (name, value) in headers {
                self.bytes
                    .extend_from_slice(format!("\r\n{name}: {value}").as_bytes());
            }
            self.bytes.extend_from_slice(b"\r\n\r\n");
            self.bytes.extend_from_slice(&body);
            self
        }

        fn raw(bytes: Vec<u8>) -> Self {
            Self { bytes }
        }
    }

    type MockHandler = Arc<dyn Fn(CapturedRequest) -> MockResponse + Send + Sync>;
    static MOCK_SERVER_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn mock_server_test_lock() -> std::sync::MutexGuard<'static, ()> {
        MOCK_SERVER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct TempFileGuard(PathBuf);

    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    struct MockServer {
        address: SocketAddr,
        stopped: Arc<AtomicBool>,
        accept_thread: Option<JoinHandle<()>>,
        workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    }

    impl MockServer {
        fn new(handler: impl Fn(CapturedRequest) -> MockResponse + Send + Sync + 'static) -> Self {
            let listener =
                TcpListener::bind(("127.0.0.1", 0)).expect("bind dynamic localhost port");
            listener
                .set_nonblocking(true)
                .expect("set mock listener nonblocking");
            let address = listener.local_addr().expect("read mock address");
            let stopped = Arc::new(AtomicBool::new(false));
            let workers = Arc::new(Mutex::new(Vec::new()));
            let server_stopped = stopped.clone();
            let server_workers = workers.clone();
            let handler: MockHandler = Arc::new(handler);
            let accept_thread = thread::spawn(move || {
                while !server_stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let handler = handler.clone();
                            let worker = thread::spawn(move || {
                                if let Some(request) = read_request(&mut stream) {
                                    let response = handler(request);
                                    let _ = stream.write_all(&response.bytes);
                                    let _ = stream.flush();
                                    if response.bytes.len() > 1024 * 1024 {
                                        thread::sleep(Duration::from_millis(20));
                                    }
                                }
                            });
                            if let Ok(mut workers) = server_workers.lock() {
                                workers.push(worker);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                address,
                stopped,
                accept_thread: Some(accept_thread),
                workers,
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.address, path)
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Release);
            let _ = TcpStream::connect(self.address);
            if let Some(thread) = self.accept_thread.take() {
                let _ = thread.join();
            }
            if let Ok(mut workers) = self.workers.lock() {
                for worker in workers.drain(..) {
                    let _ = worker.join();
                }
            }
        }
    }

    fn read_request(stream: &mut TcpStream) -> Option<CapturedRequest> {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        let header_end = loop {
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break end;
            }
            let count = stream.read(&mut chunk).ok()?;
            if count == 0 {
                return None;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if bytes.len() > 1024 * 1024 {
                return None;
            }
        };
        let header_text = String::from_utf8_lossy(&bytes[..header_end]);
        let mut lines = header_text.split("\r\n");
        let mut request_line = lines.next()?.split_whitespace();
        let method = request_line.next()?.to_owned();
        let target = request_line.next()?.to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.trim().to_owned(), value.trim().to_owned()))
            })
            .collect();
        let body_start = header_end + 4;
        let body_length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < body_start + body_length {
            let count = stream.read(&mut chunk).ok()?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        Some(CapturedRequest {
            method,
            target,
            headers,
            body: bytes[body_start..bytes.len().min(body_start + body_length)].to_vec(),
        })
    }

    fn auth_none() -> AuthConfig {
        AuthConfig {
            kind: "none".into(),
            username: String::new(),
            password: String::new(),
            token: String::new(),
            key: String::new(),
            value: String::new(),
            location: "header".into(),
        }
    }

    fn request(url: String) -> HttpRequest {
        HttpRequest {
            tab_id: "test-tab".into(),
            execution_id: format!(
                "test-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
            method: "GET".into(),
            url,
            timeout_ms: 5_000,
            params: Vec::new(),
            headers: Vec::new(),
            auth: auth_none(),
            body: RequestBody {
                kind: "none".into(),
                text: String::new(),
                fields: Vec::new(),
            },
        }
    }

    fn run_http(request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        tauri::async_runtime::block_on(execute_http(request, CancellationToken::new()))
    }

    #[test]
    fn query_params_keep_duplicates_and_encode_values() {
        let params = vec![
            KeyValue {
                key: "tag".into(),
                value: "a b".into(),
                enabled: true,
            },
            KeyValue {
                key: "tag".into(),
                value: "x&y".into(),
                enabled: true,
            },
            KeyValue {
                key: "off".into(),
                value: "ignored".into(),
                enabled: false,
            },
        ];
        let url = build_url(
            "https://example.test/path?tag=existing",
            &params,
            &auth_none(),
        )
        .unwrap();
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("tag".into(), "existing".into()),
                ("tag".into(), "a b".into()),
                ("tag".into(), "x&y".into())
            ]
        );
    }

    #[test]
    fn rejects_non_http_urls() {
        for value in ["file:///C:/secret.txt", "ftp://example.test/file"] {
            let error = build_url(value, &[], &auth_none()).unwrap_err();
            assert!(error.contains("僅支援"));
        }
        assert!(build_url("  ", &[], &auth_none())
            .unwrap_err()
            .contains("必填"));
    }

    #[test]
    fn api_key_query_is_appended_without_replacing_existing_values() {
        let auth = AuthConfig {
            kind: "apiKey".into(),
            username: String::new(),
            password: String::new(),
            token: String::new(),
            key: "key".into(),
            value: "a b".into(),
            location: "query".into(),
        };
        let url = build_url("https://example.test/path?key=existing", &[], &auth).unwrap();
        let values: Vec<String> = url
            .query_pairs()
            .filter(|(key, _)| key == "key")
            .map(|(_, value)| value.into_owned())
            .collect();
        assert_eq!(values, vec!["existing", "a b"]);
    }

    #[test]
    fn duplicate_headers_are_appended_in_order() {
        let mut headers = HeaderMap::new();
        append_header(&mut headers, "X-Trace", "first").unwrap();
        append_header(&mut headers, "X-Trace", "second").unwrap();
        let values: Vec<&str> = headers
            .get_all("x-trace")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(values, vec!["first", "second"]);
    }

    #[test]
    fn mock_http_server_accepts_every_supported_method_and_preserves_query_and_headers() {
        let _test_lock = mock_server_test_lock();
        let (sender, receiver) = mpsc::channel();
        let server = MockServer::new(move |request| {
            sender.send(request).expect("capture request");
            MockResponse::new(200, "ok")
        });

        for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
            let mut input = request(server.url("/get?existing=one"));
            input.method = method.into();
            input.params = vec![
                KeyValue {
                    key: "q".into(),
                    value: "a b".into(),
                    enabled: true,
                },
                KeyValue {
                    key: "q".into(),
                    value: "x&y".into(),
                    enabled: true,
                },
            ];
            input.headers = vec![
                KeyValue {
                    key: "X-Repeat".into(),
                    value: "first".into(),
                    enabled: true,
                },
                KeyValue {
                    key: "X-Repeat".into(),
                    value: "second".into(),
                    enabled: true,
                },
            ];
            assert_eq!(run_http(&input).unwrap().status, 200);
            let captured = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(captured.method, method);
            assert!(captured.target.contains("existing=one"));
            assert!(captured.target.contains("q=a+b"));
            assert!(captured.target.contains("q=x%26y"));
            assert_eq!(captured.header_values("x-repeat"), vec!["first", "second"]);
        }
    }

    #[test]
    fn json_text_form_and_multipart_bodies_are_sent_from_user_input() {
        let _test_lock = mock_server_test_lock();
        let (sender, receiver) = mpsc::channel();
        let server = MockServer::new(move |request| {
            sender.send(request).expect("capture request");
            MockResponse::new(200, "ok")
        });

        for (kind, text, expected_type, expected_body) in [
            (
                "json",
                "{\"name\":\"測試\"}",
                "application/json",
                "{\"name\":\"測試\"}",
            ),
            (
                "text",
                "hello 世界",
                "text/plain; charset=utf-8",
                "hello 世界",
            ),
            (
                "form",
                "",
                "application/x-www-form-urlencoded",
                "name=a+b&tag=x%26y&tag=two",
            ),
        ] {
            let mut input = request(server.url("/post"));
            input.method = "POST".into();
            input.body.kind = kind.into();
            input.body.text = text.into();
            if kind == "form" {
                input.body.fields = vec![
                    BodyField {
                        key: "name".into(),
                        value: "a b".into(),
                        enabled: true,
                        is_file: false,
                        file_path: String::new(),
                        content_type: String::new(),
                    },
                    BodyField {
                        key: "tag".into(),
                        value: "x&y".into(),
                        enabled: true,
                        is_file: false,
                        file_path: String::new(),
                        content_type: String::new(),
                    },
                    BodyField {
                        key: "tag".into(),
                        value: "two".into(),
                        enabled: true,
                        is_file: false,
                        file_path: String::new(),
                        content_type: String::new(),
                    },
                    BodyField {
                        key: "disabled".into(),
                        value: "ignored".into(),
                        enabled: false,
                        is_file: false,
                        file_path: String::new(),
                        content_type: String::new(),
                    },
                ];
            }
            assert_eq!(run_http(&input).unwrap().status, 200);
            let captured = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(captured.method, "POST");
            assert_eq!(
                captured.header_values("content-type").first(),
                Some(&expected_type)
            );
            assert_eq!(String::from_utf8(captured.body).unwrap(), expected_body);
        }

        let temp_path = std::env::temp_dir().join(format!(
            "vexa-upload-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _temp_file = TempFileGuard(temp_path.clone());
        std::fs::write(&temp_path, [0, 1, 2, 255, 42]).expect("create temporary upload fixture");
        let mut input = request(server.url("/multipart"));
        input.method = "POST".into();
        input.body.kind = "multipart".into();
        input.body.fields = vec![
            BodyField {
                key: "note".into(),
                value: "本機 fixture".into(),
                enabled: true,
                is_file: false,
                file_path: String::new(),
                content_type: String::new(),
            },
            BodyField {
                key: "upload".into(),
                value: String::new(),
                enabled: true,
                is_file: true,
                file_path: temp_path.to_string_lossy().into_owned(),
                content_type: "application/octet-stream".into(),
            },
        ];
        assert_eq!(run_http(&input).unwrap().status, 200);
        let captured = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let content_type = captured.header_values("content-type").join("");
        assert!(content_type.starts_with("multipart/form-data; boundary="));
        assert!(captured
            .body
            .windows("本機 fixture".len())
            .any(|window| window == "本機 fixture".as_bytes()));
        assert!(captured
            .body
            .windows(5)
            .any(|window| window == [0, 1, 2, 255, 42]));
    }

    #[test]
    fn basic_bearer_and_api_key_authorization_are_applied() {
        let _test_lock = mock_server_test_lock();
        let (sender, receiver) = mpsc::channel();
        let server = MockServer::new(move |request| {
            sender.send(request).expect("capture request");
            MockResponse::new(200, "ok")
        });

        for kind in ["basic", "bearer", "api-header", "api-query"] {
            let mut input = request(server.url("/headers"));
            input.auth.kind = match kind {
                "api-header" | "api-query" => "apiKey",
                other => other,
            }
            .into();
            input.auth.username = "user".into();
            input.auth.password = "pass".into();
            input.auth.token = "dummy-test-token".into();
            input.auth.key = if kind == "api-query" {
                "key".into()
            } else {
                "X-Test-Key".into()
            };
            input.auth.value = if kind == "api-header" || kind == "api-query" {
                "dummy-key".into()
            } else {
                String::new()
            };
            input.auth.location = if kind == "api-query" {
                "query".into()
            } else {
                "header".into()
            };
            assert_eq!(run_http(&input).unwrap().status, 200);
            let captured = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            match kind {
                "basic" => assert_eq!(
                    captured.header_values("authorization"),
                    vec!["Basic dXNlcjpwYXNz"]
                ),
                "bearer" => assert_eq!(
                    captured.header_values("authorization"),
                    vec!["Bearer dummy-test-token"]
                ),
                "api-header" => assert_eq!(captured.header_values("x-test-key"), vec!["dummy-key"]),
                "api-query" => {
                    assert!(captured.target.ends_with("?key=dummy-key"));
                    assert!(captured.header_values("authorization").is_empty());
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn status_codes_redirect_chains_loops_and_cookies_are_handled() {
        let _test_lock = mock_server_test_lock();
        let server = MockServer::new(|request| match request.target.as_str() {
            "/redirect" => MockResponse::new(301, "").with_headers(&[("Location", "/second")]),
            "/second" => MockResponse::new(302, "").with_headers(&[("Location", "/final")]),
            "/loop" => MockResponse::new(302, "").with_headers(&[("Location", "/loop")]),
            "/final" => MockResponse::new(201, "redirected"),
            "/cookies" => MockResponse::new(200, "ok").with_headers(&[
                ("Set-Cookie", "a=1; HttpOnly"),
                ("Set-Cookie", "b=2; Secure"),
            ]),
            _ => {
                let status = request
                    .target
                    .trim_start_matches("/status/")
                    .parse()
                    .unwrap_or(200);
                MockResponse::new(status, "status body")
            }
        });

        for status in [200, 201, 204, 400, 401, 404, 429, 500, 503] {
            let result = run_http(&request(server.url(&format!("/status/{status}")))).unwrap();
            assert_eq!(result.status, status);
            let body = BASE64.decode(result.body_base64).unwrap();
            assert_eq!(
                body,
                if status == 204 {
                    Vec::new()
                } else {
                    b"status body".to_vec()
                }
            );
        }
        let redirected = run_http(&request(server.url("/redirect"))).unwrap();
        assert_eq!(redirected.status, 201);
        assert_eq!(
            BASE64.decode(redirected.body_base64).unwrap(),
            b"redirected"
        );
        let loop_error = run_http(&request(server.url("/loop"))).unwrap_err();
        assert_eq!(loop_error.kind, "network");
        let cookies = run_http(&request(server.url("/cookies"))).unwrap();
        let set_cookie: Vec<&str> = cookies
            .headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("set-cookie"))
            .map(|header| header.value.as_str())
            .collect();
        assert_eq!(set_cookie, vec!["a=1; HttpOnly", "b=2; Secure"]);
    }

    #[test]
    fn redirects_do_not_forward_authorization_across_port() {
        let _test_lock = mock_server_test_lock();
        let (sender, receiver) = mpsc::channel();
        let destination = MockServer::new(move |request| {
            sender.send(request).expect("capture redirected request");
            MockResponse::new(200, "destination")
        });
        let location = destination.url("/destination");
        let source = MockServer::new(move |_| {
            MockResponse::new(302, "").with_headers(&[("Location", &location)])
        });
        let mut input = request(source.url("/redirect"));
        input.auth.kind = "bearer".into();
        input.auth.token = "dummy-secret-token".into();
        assert_eq!(run_http(&input).unwrap().status, 200);
        let redirected = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(redirected.header_values("authorization").is_empty());
    }

    #[test]
    fn timeout_and_cancellation_end_the_http_operation() {
        let _test_lock = mock_server_test_lock();
        let server = MockServer::new(|_| {
            thread::sleep(Duration::from_millis(300));
            MockResponse::new(200, "late")
        });
        let mut timeout_request = request(server.url("/delay/0.3"));
        timeout_request.timeout_ms = 40;
        assert_eq!(run_http(&timeout_request).unwrap_err().kind, "timeout");

        let cancel_request = request(server.url("/delay/0.3"));
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let worker = thread::spawn(move || {
            tauri::async_runtime::block_on(execute_http(&cancel_request, task_token))
        });
        thread::sleep(Duration::from_millis(40));
        cancellation.cancel();
        assert_eq!(worker.join().unwrap().unwrap_err().kind, "canceled");
    }

    #[test]
    fn response_limit_covers_below_equal_over_chunked_and_inaccurate_content_lengths() {
        let _test_lock = mock_server_test_lock();
        let limit = MAX_RESPONSE_BYTES;
        let exact = vec![b'x'; limit];
        let chunked_body = vec![b'z'; limit + 1];
        let server = MockServer::new(move |request| match request.target.as_str() {
            "/below" => MockResponse::new(200, vec![b'a'; limit - 1]),
            "/exact" => MockResponse::new(200, exact.clone()),
            "/over" => MockResponse::new(200, vec![b'b'; limit + 1]),
            "/chunked" => {
                let mut raw = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n", chunked_body.len()).into_bytes();
                raw.extend_from_slice(&chunked_body);
                raw.extend_from_slice(b"\r\n0\r\n\r\n");
                MockResponse::raw(raw)
            }
            "/lying-length" => MockResponse::raw(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcdefghij"
                    .to_vec(),
            ),
            _ => MockResponse::new(404, "missing"),
        });

        let below = run_http(&request(server.url("/below"))).unwrap();
        assert_eq!(BASE64.decode(below.body_base64).unwrap().len(), limit - 1);
        assert!(!below.truncated);
        let exact = run_http(&request(server.url("/exact"))).unwrap();
        assert_eq!(BASE64.decode(exact.body_base64).unwrap().len(), limit);
        assert!(!exact.truncated);
        let over = run_http(&request(server.url("/over"))).unwrap();
        assert_eq!(BASE64.decode(over.body_base64).unwrap().len(), limit);
        assert!(over.truncated);
        assert!(over.response_size >= (limit + 1) as u64);
        let chunked = run_http(&request(server.url("/chunked"))).unwrap();
        assert_eq!(BASE64.decode(chunked.body_base64).unwrap().len(), limit);
        assert!(chunked.truncated);
        assert!(chunked.response_size >= (limit + 1) as u64);
        // HTTP framing defines Content-Length as the body boundary. Bytes after that
        // boundary are not exposed by reqwest and cannot be interpreted as body data.
        let lying = run_http(&request(server.url("/lying-length"))).unwrap();
        assert_eq!(BASE64.decode(lying.body_base64).unwrap(), b"abcd");
        assert_eq!(lying.response_size, 4);
        assert!(!lying.truncated);
    }

    #[test]
    fn saving_response_preserves_binary_bytes_and_rejects_relative_paths() {
        let path = std::env::temp_dir().join(format!(
            "vexa-response-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _temp_file = TempFileGuard(path.clone());
        let expected = [0, 1, 2, 13, 10, 255, 42];
        tauri::async_runtime::block_on(save_response(
            path.to_string_lossy().into_owned(),
            BASE64.encode(expected),
        ))
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        let relative = tauri::async_runtime::block_on(save_response(
            "relative.bin".into(),
            BASE64.encode(expected),
        ));
        assert!(relative.is_err());
    }
}

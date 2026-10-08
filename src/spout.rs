//! # SYNGONIA Spout2 Native Output & Test Pattern Generator
//!
//! OBS / Resolume / TouchDesigner 向け Spout2 ゼロ遅延テクスチャ送信モジュール。
//! MSVC コンパイル済み `syngonia_spout.dll` C-ABI ブリッジ経由で SpoutLibrary と安全に通信。

use libloading::{Library, Symbol};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::os::raw::{c_char, c_uchar, c_uint, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const DEFAULT_SPOUT_SENDER_NAME: &str = "SYNGONIA";
pub const DEFAULT_WIDTH: u32 = 1920;
pub const DEFAULT_HEIGHT: u32 = 1080;

/// レンダー制御コマンド
pub enum SpoutCommand {
    SetAdapter(i32),
    Stop,
}

/// スレッドセーフな Spout 送信メトリクス
pub struct SpoutStats {
    pub is_active: AtomicBool,
    pub current_adapter: AtomicI32,
    pub adapter_name: std::sync::RwLock<String>,
    pub actual_sender_name: std::sync::RwLock<String>,
    pub fps: AtomicU32,
    pub frame_count: AtomicU64,
}

/// 専用 OS ネイティブレンダースレッドを統括するコントローラー
#[derive(Clone)]
pub struct SpoutController {
    sender_name: String,
    cmd_tx: Sender<SpoutCommand>,
    stats: Arc<SpoutStats>,
}

impl SpoutController {
    pub fn is_active(&self) -> bool {
        self.stats.is_active.load(Ordering::Relaxed)
    }

    pub fn get_adapter(&self) -> i32 {
        self.stats.current_adapter.load(Ordering::Relaxed)
    }

    pub fn get_adapter_name(&self) -> String {
        self.stats.adapter_name.read().unwrap().clone()
    }

    pub fn actual_name(&self) -> String {
        self.stats.actual_sender_name.read().unwrap().clone()
    }

    pub fn set_adapter(&self, index: i32) {
        let _ = self.cmd_tx.send(SpoutCommand::SetAdapter(index));
    }

    pub fn fps(&self) -> u32 {
        self.stats.fps.load(Ordering::Relaxed)
    }

    pub fn frame_count(&self) -> u64 {
        self.stats.frame_count.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> &str {
        &self.sender_name
    }
}

/// Spout 送信ステータス
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpoutStatus {
    Standby,
    Active,
    Disconnected,
    Error,
}

impl SpoutStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpoutStatus::Standby => "Standby",
            SpoutStatus::Active => "Active",
            SpoutStatus::Disconnected => "Disconnected",
            SpoutStatus::Error => "Error",
        }
    }
}

type FnSpoutInit = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type FnSpoutSendImage = unsafe extern "C" fn(*mut c_void, *const c_uchar, c_uint, c_uint) -> bool;
type FnSpoutIsInitialized = unsafe extern "C" fn(*mut c_void) -> bool;
type FnSpoutRelease = unsafe extern "C" fn(*mut c_void);
type FnSpoutSetAdapter = unsafe extern "C" fn(*mut c_void, i32) -> bool;
type FnSpoutGetAdapter = unsafe extern "C" fn(*mut c_void) -> i32;
type FnSpoutGetAdapterName = unsafe extern "C" fn(*mut c_void, *mut c_char, i32) -> bool;
type FnSpoutGetSenderName = unsafe extern "C" fn(*mut c_void, *mut c_char, i32) -> bool;

struct SpoutBridge {
    _lib: Library,
    handle: *mut c_void,
    send_image: Symbol<'static, FnSpoutSendImage>,
    is_initialized: Symbol<'static, FnSpoutIsInitialized>,
    set_adapter: Option<Symbol<'static, FnSpoutSetAdapter>>,
    get_adapter: Option<Symbol<'static, FnSpoutGetAdapter>>,
    get_adapter_name: Option<Symbol<'static, FnSpoutGetAdapterName>>,
    get_sender_name: Option<Symbol<'static, FnSpoutGetSenderName>>,
    release: Symbol<'static, FnSpoutRelease>,
}

unsafe impl Send for SpoutBridge {}
unsafe impl Sync for SpoutBridge {}

/// Spout2 センダー
pub struct SpoutSender {
    sender_name: String,
    width: u32,
    height: u32,
    status: SpoutStatus,
    bridge: Option<SpoutBridge>,
    is_active: Arc<AtomicBool>,
}

unsafe impl Send for SpoutSender {}
unsafe impl Sync for SpoutSender {}

impl SpoutSender {
    /// 新規センダー作成
    pub fn new(sender_name: &str, width: u32, height: u32) -> Self {
        let mut sender = Self {
            sender_name: sender_name.to_string(),
            width,
            height,
            status: SpoutStatus::Standby,
            bridge: None,
            is_active: Arc::new(AtomicBool::new(false)),
        };

        sender.init();
        sender
    }

    /// syngonia_spout.dll のロードと初期化
    fn init(&mut self) {
        let dll_candidates = [
            "syngonia_spout.dll",
            "target/debug/syngonia_spout.dll",
            "target/release/syngonia_spout.dll",
        ];

        let mut loaded_lib = None;
        for path_str in &dll_candidates {
            if Path::new(path_str).exists() {
                if let Ok(lib) = unsafe { Library::new(path_str) } {
                    tracing::info!("Loaded syngonia_spout.dll from {path_str}");
                    loaded_lib = Some(lib);
                    break;
                }
            }
        }

        let lib = match loaded_lib {
            Some(l) => l,
            None => {
                tracing::warn!("syngonia_spout.dll not found. Spout sender running in standby.");
                self.status = SpoutStatus::Standby;
                return;
            }
        };

        unsafe {
            let init_fn_res: Result<Symbol<FnSpoutInit>, _> = lib.get(b"Spout_Init");
            let send_fn_res: Result<Symbol<FnSpoutSendImage>, _> = lib.get(b"Spout_SendImage");
            let is_init_fn_res: Result<Symbol<FnSpoutIsInitialized>, _> = lib.get(b"Spout_IsInitialized");
            let release_fn_res: Result<Symbol<FnSpoutRelease>, _> = lib.get(b"Spout_Release");
            let set_adapter_res: Result<Symbol<FnSpoutSetAdapter>, _> = lib.get(b"Spout_SetAdapter");
            let get_adapter_res: Result<Symbol<FnSpoutGetAdapter>, _> = lib.get(b"Spout_GetAdapter");
            let get_adapter_name_res: Result<Symbol<FnSpoutGetAdapterName>, _> = lib.get(b"Spout_GetAdapterName");
            let get_sender_name_res: Result<Symbol<FnSpoutGetSenderName>, _> = lib.get(b"Spout_GetSenderName");

            if let (Ok(init_fn), Ok(send_fn), Ok(is_init_fn), Ok(release_fn)) =
                (init_fn_res, send_fn_res, is_init_fn_res, release_fn_res)
            {
                let c_name = CString::new(self.sender_name.clone()).unwrap_or_default();
                let handle = init_fn(c_name.as_ptr());

                if handle.is_null() {
                    tracing::error!("Spout_Init failed to initialize Spout library context");
                    self.status = SpoutStatus::Error;
                    return;
                }

                let send_image: Symbol<'static, FnSpoutSendImage> = std::mem::transmute(send_fn);
                let is_initialized: Symbol<'static, FnSpoutIsInitialized> = std::mem::transmute(is_init_fn);
                let release: Symbol<'static, FnSpoutRelease> = std::mem::transmute(release_fn);
                let set_adapter: Option<Symbol<'static, FnSpoutSetAdapter>> =
                    set_adapter_res.ok().map(|f| std::mem::transmute(f));
                let get_adapter: Option<Symbol<'static, FnSpoutGetAdapter>> =
                    get_adapter_res.ok().map(|f| std::mem::transmute(f));
                let get_adapter_name: Option<Symbol<'static, FnSpoutGetAdapterName>> =
                    get_adapter_name_res.ok().map(|f| std::mem::transmute(f));
                let get_sender_name: Option<Symbol<'static, FnSpoutGetSenderName>> =
                    get_sender_name_res.ok().map(|f| std::mem::transmute(f));

                self.bridge = Some(SpoutBridge {
                    _lib: lib,
                    handle,
                    send_image,
                    is_initialized,
                    set_adapter,
                    get_adapter,
                    get_adapter_name,
                    get_sender_name,
                    release,
                });

                self.status = SpoutStatus::Active;
                self.is_active.store(true, Ordering::SeqCst);
                tracing::info!("Spout2 sender '{}' is ACTIVE and ready for OBS!", self.sender_name);
            } else {
                tracing::error!("Failed to resolve exported symbols in syngonia_spout.dll");
                self.status = SpoutStatus::Error;
            }
        }
    }

    /// GPU アダプター切り替え (0: RTX 4060, 1: Iris Xe)
    pub fn set_adapter(&mut self, index: i32) -> bool {
        if let Some(ref b) = self.bridge {
            if let Some(ref set_fn) = b.set_adapter {
                unsafe { set_fn(b.handle, index) }
            } else {
                false
            }
        } else {
            false
        }
    }

    /// 現在の GPU アダプターインデックス取得
    pub fn get_adapter(&self) -> i32 {
        if let Some(ref b) = self.bridge {
            if let Some(ref get_fn) = b.get_adapter {
                unsafe { get_fn(b.handle) }
            } else {
                -1
            }
        } else {
            -1
        }
    }

    /// 現在の GPU アダプター名取得
    pub fn get_adapter_name(&self) -> String {
        if let Some(ref b) = self.bridge {
            if let Some(ref f) = b.get_adapter_name {
                let mut buf = [0u8; 256];
                let ok = unsafe { f(b.handle, buf.as_mut_ptr() as *mut c_char, 256) };
                if ok {
                    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char) };
                    return s.to_string_lossy().into_owned();
                }
            }
        }
        "Unknown".to_string()
    }

    /// 実際に登録されたセンダー名の取得
    pub fn get_real_sender_name(&self) -> String {
        if let Some(ref b) = self.bridge {
            if let Some(ref f) = b.get_sender_name {
                let mut buf = [0u8; 256];
                let ok = unsafe { f(b.handle, buf.as_mut_ptr() as *mut c_char, 256) };
                if ok {
                    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char) };
                    return s.to_string_lossy().into_owned();
                }
            }
        }
        self.sender_name.clone()
    }

    /// 映像フレーム（RGBA ピクセルバッファ）の送信
    pub fn send_frame(&mut self, rgba_pixels: &[u8], width: u32, height: u32) -> bool {
        if let Some(ref b) = self.bridge {
            unsafe {
                let success = (b.send_image)(b.handle, rgba_pixels.as_ptr(), width, height);
                if success {
                    self.status = SpoutStatus::Active;
                    true
                } else {
                    self.status = SpoutStatus::Disconnected;
                    false
                }
            }
        } else {
            false
        }
    }

    /// センダーの初期化状態確認
    pub fn is_initialized(&self) -> bool {
        if let Some(ref b) = self.bridge {
            unsafe { (b.is_initialized)(b.handle) }
        } else {
            false
        }
    }

    /// センダーのステータス取得
    pub fn status(&self) -> SpoutStatus {
        self.status
    }

    /// センダー名の取得
    pub fn name(&self) -> &str {
        &self.sender_name
    }
}

impl Drop for SpoutSender {
    fn drop(&mut self) {
        if let Some(ref b) = self.bridge {
            unsafe {
                (b.release)(b.handle);
            }
        }
    }
}

/// 現場疎通確認用 SMPTE カラーバー & 動くスキャンバーの生成 (ゼロアロケーション高速描画)
/// - 7色 SMPTE バー（上部 70%）
/// - グラデーション・グレーバー（中間 15%）
/// - 動くバウンシング・ブロック & センタークロスヘア（下部 15%）
/// これにより OBS / プロジェクター上で「映像受信」「FPS動作」「解像度・アスペクト比」が一目で確認可能。
pub fn render_test_pattern_inplace(pixels: &mut [u8], frame_num: u64, width: u32, height: u32) {
    let expected_len = (width * height * 4) as usize;
    if pixels.len() < expected_len {
        return;
    }

    // SMPTE 7色カラーバー
    let smpte_colors: [(u8, u8, u8); 7] = [
        (192, 192, 192), // 75% White
        (192, 192, 0),   // Yellow
        (0, 192, 192),   // Cyan
        (0, 192, 0),     // Green
        (192, 0, 192),   // Magenta
        (192, 0, 0),     // Red
        (0, 0, 192),     // Blue
    ];

    let bar_width = width / 7;
    let top_h = (height as f32 * 0.70) as u32;
    let mid_h = (height as f32 * 0.85) as u32;

    // 下部バウンシングブロックの位置計算 (0..widthの間をピンポン往復)
    let cycle = (width - 120) as u64;
    let block_pos = if cycle > 0 {
        let phase = (frame_num * 6) % (cycle * 2);
        if phase < cycle { phase as u32 } else { (cycle * 2 - phase) as u32 }
    } else {
        0
    };

    for y in 0..height {
        let is_top = y < top_h;
        let is_mid = y >= top_h && y < mid_h;
        let row_offset = (y * width * 4) as usize;

        for x in 0..width {
            let px_idx = row_offset + (x * 4) as usize;

            if is_top {
                let color_idx = ((x / bar_width) as usize).min(6);
                let (r, g, b) = smpte_colors[color_idx];
                // BGRA ordering for DXGI_FORMAT_B8G8R8A8_UNORM
                pixels[px_idx] = b;
                pixels[px_idx + 1] = g;
                pixels[px_idx + 2] = r;
                pixels[px_idx + 3] = 255;
            } else if is_mid {
                // グレー階調 (左0% -> 右100%)
                let gray = ((x as f32 / width as f32) * 255.0) as u8;
                pixels[px_idx] = gray;
                pixels[px_idx + 1] = gray;
                pixels[px_idx + 2] = gray;
                pixels[px_idx + 3] = 255;
            } else {
                // 下部エリア: 黒背景 + 動くシアンブロック + タイムコード風点滅
                let in_moving_block = x >= block_pos && x < (block_pos + 120);
                if in_moving_block {
                    // 動くブロック (シアン: B=255, G=255, R=0)
                    pixels[px_idx] = 255;
                    pixels[px_idx + 1] = 255;
                    pixels[px_idx + 2] = 0;
                    pixels[px_idx + 3] = 255;
                } else {
                    // 1秒周期のビート点滅インジケーター（右端: 赤 B=50, G=50, R=255）
                    let in_beat_box = x > width - 100 && y > height - 60;
                    let beat_lit = (frame_num % 60) < 15;
                    if in_beat_box && beat_lit {
                        pixels[px_idx] = 50;
                        pixels[px_idx + 1] = 50;
                        pixels[px_idx + 2] = 255;
                        pixels[px_idx + 3] = 255;
                    } else {
                        // 濃いネイビー背景 (B=28, G=20, R=16)
                        pixels[px_idx] = 28;
                        pixels[px_idx + 1] = 20;
                        pixels[px_idx + 2] = 16;
                        pixels[px_idx + 3] = 255;
                    }
                }
            }

            // センタークロスヘア (画面中心の1pxグリッド線)
            if (x == width / 2 || y == height / 2) && (x % 4 < 2) {
                pixels[px_idx] = 255;
                pixels[px_idx + 1] = 255;
                pixels[px_idx + 2] = 255;
                pixels[px_idx + 3] = 255;
            }
        }
    }
}

/// 既存コード互換用テストパターン生成
pub fn generate_test_pattern(frame_num: u64, width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    render_test_pattern_inplace(&mut pixels, frame_num, width, height);
    pixels
}

/// 専用 OS ネイティブスレッドで Spout2 送信ループを起動
pub fn spawn_spout_render_thread(
    sender_name: &str,
    width: u32,
    height: u32,
    state_manager: Arc<crate::state::StateManager>,
) -> SpoutController {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let stats = Arc::new(SpoutStats {
        is_active: AtomicBool::new(false),
        current_adapter: AtomicI32::new(0),
        adapter_name: std::sync::RwLock::new("Detecting...".to_string()),
        actual_sender_name: std::sync::RwLock::new(sender_name.to_string()),
        fps: AtomicU32::new(0),
        frame_count: AtomicU64::new(0),
    });

    let thread_stats = Arc::clone(&stats);
    let name_string = sender_name.to_string();

    std::thread::Builder::new()
        .name("syngonia-spout-render".to_string())
        .spawn(move || {
            tracing::info!(
                "[SPOUT RENDER THREAD] Starting dedicated DirectX 11 render loop for '{}' ({width}x{height})",
                name_string
            );

            // 専用スレッド内で SpoutSender を初期化（DirectX 11 デバイスコンテキストのスレッド親和性を完全に保証）
            let mut sender = SpoutSender::new(&name_string, width, height);
            let initial_adapter = sender.get_adapter();
            let adapter_name = sender.get_adapter_name();
            let actual_name = sender.get_real_sender_name();

            thread_stats.current_adapter.store(initial_adapter, Ordering::Relaxed);
            *thread_stats.adapter_name.write().unwrap() = adapter_name;
            *thread_stats.actual_sender_name.write().unwrap() = actual_name;
            thread_stats.is_active.store(sender.is_initialized(), Ordering::Relaxed);

            let mut frame_count: u64 = 0;
            // ゼロアロケーション用 固定バッファ
            let mut frame_buffer = vec![0u8; (width * height * 4) as usize];
            let target_frame_duration = Duration::from_micros(16_666); // 60.0 fps (16.666 ms)

            let mut last_fps_check = Instant::now();
            let mut fps_counter = 0u32;

            loop {
                let frame_start = Instant::now();

                // 1. スレッドセーフなコマンド処理（GPU切り替え / 停止等）
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        SpoutCommand::SetAdapter(idx) => {
                            tracing::info!("[SPOUT RENDER THREAD] Switching Spout GPU Adapter to [{idx}]...");
                            sender.set_adapter(idx);
                            thread_stats.current_adapter.store(sender.get_adapter(), Ordering::Relaxed);
                            *thread_stats.adapter_name.write().unwrap() = sender.get_adapter_name();
                        }
                        SpoutCommand::Stop => {
                            tracing::info!("[SPOUT RENDER THREAD] Received Stop command, exiting loop.");
                            return;
                        }
                    }
                }

                // 2. フラグ取得 (0 ナノ秒 ロックフリー)
                let is_blackout = state_manager.is_blackout();
                let is_test_pattern = state_manager.is_test_pattern();

                frame_count += 1;
                thread_stats.frame_count.store(frame_count, Ordering::Relaxed);

                // 3. ピクセル生成 (ゼロアロケーション) & Direct3D 11 共有テクスチャへ送信
                if is_blackout {
                    for pixel in frame_buffer.chunks_exact_mut(4) {
                        pixel[0] = 0;
                        pixel[1] = 0;
                        pixel[2] = 0;
                        pixel[3] = 255;
                    }
                } else if is_test_pattern {
                    render_test_pattern_inplace(&mut frame_buffer, frame_count, width, height);
                } else {
                    // 通常モード: アンビエントパルス (本番エフェクト待機中)
                    let t = frame_count as f32 * 0.03;
                    let r = ((t.sin() * 0.5 + 0.5) * 40.0) as u8;
                    let g = (((t + 2.0).sin() * 0.5 + 0.5) * 30.0) as u8;
                    let b = (((t + 4.0).sin() * 0.5 + 0.5) * 80.0 + 20.0) as u8;
                    // BGRA order for DXGI_FORMAT_B8G8R8A8_UNORM
                    for pixel in frame_buffer.chunks_exact_mut(4) {
                        pixel[0] = b;
                        pixel[1] = g;
                        pixel[2] = r;
                        pixel[3] = 255;
                    }
                }

                let sent = sender.send_frame(&frame_buffer, width, height);
                thread_stats.is_active.store(sent && sender.is_initialized(), Ordering::Relaxed);

                // 4. FPS 計測
                fps_counter += 1;
                if last_fps_check.elapsed() >= Duration::from_secs(1) {
                    thread_stats.fps.store(fps_counter, Ordering::Relaxed);
                    fps_counter = 0;
                    last_fps_check = Instant::now();
                }

                // 5. 60fps ペーシング（高精度スリープ）
                let elapsed = frame_start.elapsed();
                if elapsed < target_frame_duration {
                    std::thread::sleep(target_frame_duration - elapsed);
                }
            }
        })
        .expect("Failed to spawn syngonia-spout-render thread");

    SpoutController {
        sender_name: sender_name.to_string(),
        cmd_tx,
        stats,
    }
}

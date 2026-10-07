//! # SYNGONIA Spout2 Native Output
//!
//! OBS / Resolume / TouchDesigner 向け Spout2 ゼロ遅延テクスチャ送信モジュール。
//!
//! ## 設計原則（SYNGONIA-20261007-DESIGN 準拠）
//! - **センダー名固定**: `"SYNGONIA"`（OBS の Spout2 Capture ソースで即時自動認識）
//! - **切断時も描画継続**: OBS の再起動やシーン切替で切断されてもアプリは絶対に終了しない。
//! - **動的リンク**: `SpoutLibrary.dll` が見つからない環境でもクラッシュせず安全フォールバック。

use libloading::{Library, Symbol};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::os::raw::{c_char, c_uchar, c_uint, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const DEFAULT_SPOUT_SENDER_NAME: &str = "SYNGONIA";
pub const DEFAULT_WIDTH: u32 = 1920;
pub const DEFAULT_HEIGHT: u32 = 1080;

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

// C++ vtable 用の関数ポインタ型定義 (x86_64 MSVC ABI: 第一引数は this ポインタ)
type FnSetSenderName = unsafe extern "C" fn(*mut c_void, *const c_char);
type FnReleaseSender = unsafe extern "C" fn(*mut c_void, u32);
type FnSendImage = unsafe extern "C" fn(
    *mut c_void,
    *const c_uchar, // pixels
    c_uint,         // width
    c_uint,         // height
    c_uint,         // glFormat (0x1908 = GL_RGBA)
    bool,           // bInvert
) -> bool;
type FnIsInitialized = unsafe extern "C" fn(*mut c_void) -> bool;
type FnGetFps = unsafe extern "C" fn(*mut c_void) -> f64;
type FnGetHandle = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type FnRelease = unsafe extern "C" fn(*mut c_void);

/// Spout vtable ラッパー構造体
struct SpoutVtable {
    instance: *mut c_void,
    set_sender_name: FnSetSenderName,
    release_sender: FnReleaseSender,
    send_image: FnSendImage,
    is_initialized: FnIsInitialized,
    get_fps: FnGetFps,
    get_handle: FnGetHandle,
    release: FnRelease,
}

unsafe impl Send for SpoutVtable {}
unsafe impl Sync for SpoutVtable {}

/// Spout2 センダー
pub struct SpoutSender {
    sender_name: String,
    width: u32,
    height: u32,
    status: SpoutStatus,
    _lib: Option<Library>,
    vtable: Option<SpoutVtable>,
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
            _lib: None,
            vtable: None,
            is_active: Arc::new(AtomicBool::new(false)),
        };

        sender.init_library();
        sender
    }

    /// SpoutLibrary.dll のロードと vtable の初期化
    fn init_library(&mut self) {
        let dll_candidates = [
            "SpoutLibrary.dll",
            "target/debug/SpoutLibrary.dll",
            "target/release/SpoutLibrary.dll",
            "third_party/spout/Spout-SDK-binaries/Libs_2-007-017/MD/bin/SpoutLibrary.dll",
        ];

        let mut loaded_lib = None;
        for path_str in &dll_candidates {
            if Path::new(path_str).exists() {
                if let Ok(lib) = unsafe { Library::new(path_str) } {
                    tracing::info!("Loaded SpoutLibrary.dll from {path_str}");
                    loaded_lib = Some(lib);
                    break;
                }
            }
        }

        let lib = match loaded_lib {
            Some(l) => l,
            None => {
                tracing::warn!("SpoutLibrary.dll not found. Spout sender will run in safe virtual mode.");
                self.status = SpoutStatus::Standby;
                return;
            }
        };

        unsafe {
            // GetSpout ファクトリ関数取得
            let get_spout_res: Result<Symbol<unsafe extern "C" fn() -> *mut c_void>, _> =
                lib.get(b"GetSpout");

            let get_spout = match get_spout_res {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("Failed to get symbol GetSpout: {e}");
                    self.status = SpoutStatus::Error;
                    return;
                }
            };

            let instance = get_spout();
            if instance.is_null() {
                tracing::warn!("GetSpout returned null instance");
                self.status = SpoutStatus::Error;
                return;
            }

            // vtable の解決 (C++ 仮想関数テーブルのオフセット読み取り)
            let vtable_ptr = *(instance as *const *const *const c_void);

            // インデックス順序 (SpoutLibrary.h 準拠)
            // 0: SetSenderName
            // 2: ReleaseSender
            // 5: SendImage
            // 6: IsInitialized
            // 10: GetFps
            // 12: GetHandle
            // 43: Release (末尾)
            let set_sender_name: FnSetSenderName = std::mem::transmute(*vtable_ptr.add(0));
            let release_sender: FnReleaseSender = std::mem::transmute(*vtable_ptr.add(2));
            let send_image: FnSendImage = std::mem::transmute(*vtable_ptr.add(5));
            let is_initialized: FnIsInitialized = std::mem::transmute(*vtable_ptr.add(6));
            let get_fps: FnGetFps = std::mem::transmute(*vtable_ptr.add(10));
            let get_handle: FnGetHandle = std::mem::transmute(*vtable_ptr.add(12));
            let release: FnRelease = std::mem::transmute(*vtable_ptr.add(43));

            let c_name = CString::new(self.sender_name.clone()).unwrap_or_default();
            set_sender_name(instance, c_name.as_ptr());

            self.vtable = Some(SpoutVtable {
                instance,
                set_sender_name,
                release_sender,
                send_image,
                is_initialized,
                get_fps,
                get_handle,
                release,
            });

            self._lib = Some(lib);
            self.status = SpoutStatus::Active;
            self.is_active.store(true, Ordering::SeqCst);
            tracing::info!("Spout2 sender '{}' registered successfully!", self.sender_name);
        }
    }

    /// 映像フレーム（RGBA ピクセルバッファ）の送信
    pub fn send_frame(&mut self, rgba_pixels: &[u8], width: u32, height: u32) -> bool {
        if let Some(ref vt) = self.vtable {
            unsafe {
                let success = (vt.send_image)(
                    vt.instance,
                    rgba_pixels.as_ptr(),
                    width,
                    height,
                    0x1908, // GL_RGBA
                    false,  // bInvert
                );

                if success {
                    self.status = SpoutStatus::Active;
                    true
                } else {
                    self.status = SpoutStatus::Disconnected;
                    false
                }
            }
        } else {
            // ライブラリ未ロード時は安全にスルー（描画を止めない）
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
        if let Some(ref vt) = self.vtable {
            unsafe {
                (vt.release_sender)(vt.instance, 0);
                (vt.release)(vt.instance);
            }
        }
    }
}

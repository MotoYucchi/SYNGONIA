//! # SYNGONIA Core Executable
//!
//! プロユース現場特化型 リアルタイムVJプラットフォーム。
//!
//! ## 接続方法
//! 1. PC でモバイルホットスポットを ON にする（通常 192.168.137.1）。
//! 2. iPad を Hotspot の Wi-Fi に接続する。
//! 3. iPad の Safari で `http://192.168.137.1/touch`（または表示されたURL）を開く。
//! 4. OBS の Spout2 Capture ソースで "SYNGONIA" を選択して取り込む。
//! 5. 外部プロジェクター / OBS Browser Source で `http://localhost/viewer` を全画面表示する。

mod audio;
mod network;
mod spout;
mod state;
mod ws;

use audio::AudioDspEngine;
use network::NetworkManager;
use spout::{spawn_spout_render_thread, SpoutController};
use state::StateManager;
use ws::WsManager;

use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use axum::{Json, Router};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

#[derive(Clone)]
struct AppContext {
    state_manager: Arc<StateManager>,
    ws_manager: WsManager,
    spout_controller: SpoutController,
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ログ初期化
    tracing_subscriber::fmt()
        .with_env_filter("syngonia=info,tower_http=info")
        .init();

    println!(r#"
  ███████╗██╗   ██╗███╗   ██╗ ██████╗  ██████╗ ███╗   ██╗██╗ █████╗ 
  ██╔════╝╚██╗ ██╔╝████╗  ██║██╔════╝ ██╔═══██╗████╗  ██║██║██╔══██╗
  ███████╗ ╚████╔╝ ██╔██╗ ██║██║  ███╗██║   ██║██╔██╗ ██║██║███████║
  ╚════██║  ╚██╔╝  ██║╚██╗██║██║   ██║██║   ██║██║╚██╗██║██║██╔══██║
  ███████║   ██║   ██║ ╚████║╚██████╔╝╚██████╔╝██║ ╚████║██║██║  ██║
  ╚══════╝   ╚═╝   ╚═╝  ╚═══╝ ╚═════╝  ╚═════╝ ╚═╝  ╚═══╝╚═╝╚═╝  ╚═╝
  -- Realtime Live VJ Platform for Professionals --
    "#);

    // データ保存ディレクトリ
    let app_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let storage_dir = app_dir.join("storage");
    let web_dir = app_dir.join("web");
    std::fs::create_dir_all(&storage_dir).ok();
    std::fs::create_dir_all(&web_dir).ok();

    // 1. 状態管理エンジン初期化 (Single Source of Truth)
    let state_manager = Arc::new(StateManager::new(&storage_dir));

    // 2. WASAPI Loopback オーディオエンジン初期化 (512band FFT & Flywheel BPM)
    let (audio_engine, audio_rx) = AudioDspEngine::new(64);
    audio_engine.start();

    // 3. Spout2 専用 OS ネイティブ レンダースレッド起動 ("SYNGONIA" 1080p 60fps ロックソリッド送信)
    // Tokio ワーカースレッドプールから完全分離し、Direct3D 11 スレッド親和性と 0 ジッター送出を保証
    let spout_controller = spawn_spout_render_thread(
        "SYNGONIA",
        1920,
        1080,
        Arc::clone(&state_manager),
    );

    // 4. ポート選択: 80 を試行、失敗時は 29117 にフォールバック
    let (listener, port) = bind_listener().await?;

    // 5. WebSocket トランスポート初期化
    let ws_manager = WsManager::new(Arc::clone(&state_manager), audio_rx);

    let context = AppContext {
        state_manager,
        ws_manager: ws_manager.clone(),
        spout_controller,
        port,
    };

    // 6. Axum ルーター構築
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/", get(handle_root_redirect))
        .route("/touch", get(serve_touch_mixer))
        .route("/pc", get(serve_pc_master))
        .route("/viewer", get(serve_viewer))
        .route("/api/endpoints", get(handle_get_endpoints))
        .route("/api/spout_status", get(handle_get_spout_status))
        .route("/api/spout_adapter", get(handle_get_spout_adapter).post(handle_set_spout_adapter))
        .route("/api/test_pattern", get(handle_toggle_test_pattern).post(handle_toggle_test_pattern))
        .nest_service("/static", ServeDir::new(&web_dir))
        .layer(CorsLayer::permissive())
        .with_state(context);

    // 接続先 URL の案内バナー表示
    let endpoints = NetworkManager::get_endpoints(port);
    println!("------------------------------------------------------------");
    println!("  [SYNGONIA SERVER ONLINE] Port: {port}");
    println!("  [SPOUT2 SENDER ACTIVE] Sender Name: \"SYNGONIA\" (1080p 120fps)");
    println!("------------------------------------------------------------");
    for ep in &endpoints {
        if ep.is_hotspot {
            println!("  [HOTSPOT (iPad 最優先接続)]");
            println!("  >>> Touch Mixer : {}", ep.touch_url);
            println!("  >>> PC Console  : {}", ep.pc_url);
            println!("  >>> Screen Out  : {}", ep.viewer_url);
            println!();
        } else {
            println!("  [LAN / Wi-Fi: {}]", ep.ip);
            println!("  >>> Touch Mixer : {}", ep.touch_url);
        }
    }
    println!("------------------------------------------------------------\n");

    axum::serve(listener, app).await?;

    Ok(())
}

/// ポートバインド試行 (80 -> 29117)
async fn bind_listener() -> Result<(tokio::net::TcpListener, u16), Box<dyn std::error::Error>> {
    for port in [80, 29117] {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => {
                tracing::info!("Bound server to 0.0.0.0:{port}");
                return Ok((l, port));
            }
            Err(e) => {
                tracing::warn!("Port {port} unavailable ({e}), trying next...");
            }
        }
    }
    let addr = SocketAddr::from(([0, 0, 0, 0], 0));
    let l = tokio::net::TcpListener::bind(addr).await?;
    let port = l.local_addr()?.port();
    Ok((l, port))
}

/// ルートアクセス時のスマートリダイレクト
async fn handle_root_redirect(
    axum::extract::Host(_host): axum::extract::Host,
) -> impl IntoResponse {
    Redirect::temporary("/touch")
}

/// iPad 向け Touch Mixer EX 配信
async fn serve_touch_mixer() -> impl IntoResponse {
    let path = PathBuf::from("web/touch.html");
    if path.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            return Html(content).into_response();
        }
    }
    Html(include_str!("../web/touch.html")).into_response()
}

/// PC Master GUI 配信
async fn serve_pc_master() -> impl IntoResponse {
    let path = PathBuf::from("web/pc.html");
    if path.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            return Html(content).into_response();
        }
    }
    Html(include_str!("../web/pc.html")).into_response()
}

/// 映像出力 Viewer 配信
async fn serve_viewer() -> impl IntoResponse {
    let path = PathBuf::from("web/viewer.html");
    if path.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            return Html(content).into_response();
        }
    }
    Html(include_str!("../web/viewer.html")).into_response()
}

/// 接続エンドポイント一覧 API
async fn handle_get_endpoints(
    State(ctx): State<AppContext>,
) -> impl IntoResponse {
    let endpoints = NetworkManager::get_endpoints(ctx.port);
    let json_list: Vec<serde_json::Value> = endpoints
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "ip": e.ip,
                "port": e.port,
                "is_hotspot": e.is_hotspot,
                "touch_url": e.touch_url,
                "pc_url": e.pc_url,
                "viewer_url": e.viewer_url,
            })
        })
        .collect();

    Json(json_list)
}

/// Spout ステータス取得 API
async fn handle_get_spout_status(
    State(ctx): State<AppContext>,
) -> impl IntoResponse {
    let is_init = ctx.spout_controller.is_active();
    let adapter = ctx.spout_controller.get_adapter();
    let adapter_name = ctx.spout_controller.get_adapter_name();
    let actual_sender = ctx.spout_controller.actual_name();
    Json(serde_json::json!({
        "sender_name": actual_sender,
        "status": if is_init { "Active" } else { "Standby" },
        "initialized": is_init,
        "fps": ctx.spout_controller.fps(),
        "frame_count": ctx.spout_controller.frame_count(),
        "adapter_index": adapter,
        "adapter_name": adapter_name
    }))
}

#[derive(serde::Deserialize)]
struct AdapterQuery {
    index: Option<i32>,
}

/// GPU アダプター切り替え API (GET: 取得, POST: 切り替え)
async fn handle_get_spout_adapter(
    State(ctx): State<AppContext>,
) -> impl IntoResponse {
    let adapter = ctx.spout_controller.get_adapter();
    Json(serde_json::json!({
        "current_adapter": adapter,
        "adapters": [
            { "index": 0, "name": "NVIDIA GeForce RTX 4060 Laptop GPU (High Perf)" },
            { "index": 1, "name": "Intel(R) Iris(R) Xe Graphics (Integrated)" }
        ]
    }))
}

async fn handle_set_spout_adapter(
    State(ctx): State<AppContext>,
    axum::extract::Query(query): axum::extract::Query<AdapterQuery>,
) -> impl IntoResponse {
    let target_idx = query.index.unwrap_or(0);
    ctx.spout_controller.set_adapter(target_idx);
    Json(serde_json::json!({
        "success": true,
        "target_adapter": target_idx,
        "message": format!("Sent adapter switch command [{target_idx}] to dedicated render thread")
    }))
}

/// 現場疎通テストパターントグル API
async fn handle_toggle_test_pattern(
    State(ctx): State<AppContext>,
) -> impl IntoResponse {
    let current = ctx.state_manager.is_test_pattern();
    let new_val = !current;
    ctx.state_manager
        .update_param("deck-a", None, None, "test_pattern", crate::state::ParamValue::Bool(new_val))
        .await;

    Json(serde_json::json!({
        "test_pattern": new_val,
        "message": if new_val { "Test pattern (SMPTE bars) enabled" } else { "Test pattern disabled" }
    }))
}

/// WebSocket アップグレードハンドラ
async fn ws_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    State(ctx): State<AppContext>,
) -> impl IntoResponse {
    ctx.ws_manager.handle_upgrade(ws).await
}

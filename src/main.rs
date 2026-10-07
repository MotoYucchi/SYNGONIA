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
use spout::SpoutSender;
use state::StateManager;
use ws::WsManager;

use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use axum::{Json, Router};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

#[derive(Clone)]
struct AppContext {
    state_manager: Arc<StateManager>,
    ws_manager: WsManager,
    spout_sender: Arc<Mutex<SpoutSender>>,
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

    // 3. Spout2 センダー初期化 ("SYNGONIA" 1080p ゼロ遅延送信)
    let spout_sender = Arc::new(Mutex::new(SpoutSender::new("SYNGONIA", 1920, 1080)));

    // バックグラウンド Spout2 フレーム送信ループ (60fps)
    let spout_clone = Arc::clone(&spout_sender);
    let state_clone = Arc::clone(&state_manager);
    tokio::spawn(async move {
        let width = 1920usize;
        let height = 1080usize;
        let mut frame_buffer = vec![0u8; width * height * 4];
        let mut time = 0.0f32;

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
            time += 0.016;

            // スナップショット状態の取得
            let is_blackout = state_clone.get_snapshot().await.master.blackout;

            if is_blackout {
                frame_buffer.fill(0); // 完全暗転
            } else {
                // テスト用リアルタイムカラーパルス
                let pulse = (time * 2.0).sin().abs();
                let r = (pulse * 20.0) as u8;
                let g = ((1.0 - pulse) * 30.0) as u8;
                let b = 50u8;

                // バッファクリア
                for pixel in frame_buffer.chunks_exact_mut(4) {
                    pixel[0] = r;
                    pixel[1] = g;
                    pixel[2] = b;
                    pixel[3] = 255;
                }
            }

            if let Ok(mut sender) = spout_clone.try_lock() {
                sender.send_frame(&frame_buffer, width as u32, height as u32);
            }
        }
    });

    // 4. ポート選択: 80 を試行、失敗時は 29117 にフォールバック
    let (listener, port) = bind_listener().await?;

    // 5. WebSocket トランスポート初期化
    let ws_manager = WsManager::new(Arc::clone(&state_manager), audio_rx);

    let context = AppContext {
        state_manager,
        ws_manager: ws_manager.clone(),
        spout_sender: Arc::clone(&spout_sender),
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
    let sender = ctx.spout_sender.lock().await;
    Json(serde_json::json!({
        "sender_name": sender.name(),
        "status": sender.status().as_str(),
    }))
}

/// WebSocket アップグレードハンドラ
async fn ws_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    State(ctx): State<AppContext>,
) -> impl IntoResponse {
    ctx.ws_manager.handle_upgrade(ws).await
}

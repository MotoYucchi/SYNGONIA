//! # SYNGONIA WebSocket Transport
//!
//! リアルタイム状態同期 & 512band FFT オーディオ配信 WebSocket サーバー。
//!
//! ## 設計原則（SYNGONIA-20261007-DESIGN 準拠）
//! - **エコーバック破棄**: `client_id` により、自分が操作した値が返ってきてフェーダーが震える現象を完全根絶。
//! - **SnapSync**: クライアント接続時に 0.1 秒以内に完全スナップショットを配信。
//! - **高並行・ゼロブロック**: オーディオバイナリとJSON制御メッセージを効率的に多重配信。

use crate::state::{GroupDef, ParamValue, StateManager, SyngoniaState};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

/// 受信/送信するJSONメッセージの仕様
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsMessage {
    /// 完全スナップショット同期要求
    #[serde(rename = "request_sync")]
    RequestSync { client_id: Option<String> },

    /// 完全スナップショット配信 (SnapSync)
    #[serde(rename = "snap_sync")]
    SnapSync { state: SyngoniaState },

    /// レイヤー再構築 (MotoVJ 互換)
    #[serde(rename = "rebuild_layers")]
    RebuildLayers {
        deckId: String,
        layers: Vec<GroupDef>,
        client_id: Option<String>,
    },

    /// 単一パラメータ更新 (MotoVJ 互換 + client_id 拡張)
    #[serde(rename = "param_update")]
    ParamUpdate {
        deckId: String,
        groupId: Option<String>,
        layerId: Option<String>,
        paramId: String,
        value: ParamValue,
        client_id: Option<String>,
    },

    /// 緊急パニックアクション
    #[serde(rename = "panic_action")]
    PanicAction {
        action: String, // "blackout", "freeze", "fx_kill"
        value: Option<bool>,
        client_id: Option<String>,
    },
}

/// WebSocket ブロードキャストマネージャー
#[derive(Clone)]
pub struct WsManager {
    state_manager: Arc<StateManager>,
    broadcast_tx: broadcast::Sender<(String, Option<String>)>, // (json, sender_client_id)
    audio_rx_factory: Arc<tokio::sync::Mutex<broadcast::Receiver<Vec<u8>>>>,
}

impl WsManager {
    pub fn new(
        state_manager: Arc<StateManager>,
        audio_rx: broadcast::Receiver<Vec<u8>>,
    ) -> Self {
        let (broadcast_tx, _) = broadcast::channel(1024);
        Self {
            state_manager,
            broadcast_tx,
            audio_rx_factory: Arc::new(tokio::sync::Mutex::new(audio_rx)),
        }
    }

    /// WebSocket アップグレードハンドラ
    pub async fn handle_upgrade(
        self,
        ws: WebSocketUpgrade,
    ) -> impl IntoResponse {
        ws.on_upgrade(move |socket| self.handle_socket(socket))
    }

    /// ソケットライフサイクル
    async fn handle_socket(self, socket: WebSocket) {
        let client_id = Uuid::new_v4().to_string();
        tracing::info!("New WebSocket client connected: {client_id}");

        let (mut ws_sender, mut ws_receiver) = socket.split();
        let (client_tx, mut client_rx) = mpsc::channel::<Message>(256);

        // 1. 接続直後：即座に完全スナップショット (SnapSync) を配信
        let initial_snapshot = self.state_manager.get_snapshot().await;
        let snap_msg = WsMessage::SnapSync {
            state: initial_snapshot.clone(),
        };
        if let Ok(json) = serde_json::to_string(&snap_msg) {
            let _ = ws_sender.send(Message::Text(json)).await;
        }

        // また、MotoVJ 互換の rebuild_layers メッセージも初期送信
        for (deck_id, layers) in initial_snapshot.decks.iter() {
            let moto_msg = serde_json::json!({
                "type": "rebuild_layers",
                "deckId": deck_id,
                "layers": layers,
            });
            let _ = ws_sender.send(Message::Text(moto_msg.to_string())).await;
        }

        // 2. 送信ループ（client_rx -> ws_sender）
        let send_task = tokio::spawn(async move {
            while let Some(msg) = client_rx.recv().await {
                if ws_sender.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // 3. ブロードキャスト購読（他クライアントからの制御メッセージを受信）
        let mut b_rx = self.broadcast_tx.subscribe();
        let my_client_id = client_id.clone();
        let client_tx_clone = client_tx.clone();

        let broadcast_task = tokio::spawn(async move {
            while let Ok((json, sender_id)) = b_rx.recv().await {
                // エコーバック破棄：発信元クライアントには送り返さない！
                if let Some(ref sid) = sender_id {
                    if sid == &my_client_id {
                        continue;
                    }
                }
                if client_tx_clone.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        });

        // 4. オーディオバイナリ転送ループ（512band FFT を毎フレーム送出）
        let client_tx_audio = client_tx.clone();
        let mut audio_rx = {
            let guard = self.audio_rx_factory.lock().await;
            guard.resubscribe()
        };

        let audio_task = tokio::spawn(async move {
            while let Ok(bytes) = audio_rx.recv().await {
                if client_tx_audio.send(Message::Binary(bytes)).await.is_err() {
                    break;
                }
            }
        });

        // 5. 受信ループ（クライアントからの操作を受信）
        let state_manager = Arc::clone(&self.state_manager);
        let broadcast_tx = self.broadcast_tx.clone();
        let client_id_in_recv = client_id.clone();

        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(ws_msg) = serde_json::from_str::<WsMessage>(&text) {
                        match ws_msg {
                            WsMessage::RequestSync { .. } => {
                                let snapshot = state_manager.get_snapshot().await;
                                let reply = WsMessage::SnapSync { state: snapshot };
                                if let Ok(json) = serde_json::to_string(&reply) {
                                    let _ = client_tx.send(Message::Text(json)).await;
                                }
                            }
                            WsMessage::ParamUpdate {
                                deckId,
                                groupId,
                                layerId,
                                paramId,
                                value,
                                ..
                            } => {
                                state_manager
                                    .update_param(
                                        &deckId,
                                        groupId.as_deref(),
                                        layerId.as_deref(),
                                        &paramId,
                                        value.clone(),
                                    )
                                    .await;

                                // 他クライアントへブロードキャスト（発信元IDを付与）
                                let out_msg = serde_json::json!({
                                    "type": "param_update",
                                    "deckId": deckId,
                                    "groupId": groupId,
                                    "layerId": layerId,
                                    "paramId": paramId,
                                    "value": value,
                                    "client_id": client_id_in_recv,
                                });
                                let _ = broadcast_tx.send((out_msg.to_string(), Some(client_id_in_recv.clone())));
                            }
                            WsMessage::RebuildLayers {
                                deckId,
                                layers,
                                ..
                            } => {
                                state_manager.rebuild_layers(&deckId, layers.clone()).await;
                                state_manager.save_to_disk().await;

                                let out_msg = serde_json::json!({
                                    "type": "rebuild_layers",
                                    "deckId": deckId,
                                    "layers": layers,
                                    "client_id": client_id_in_recv,
                                });
                                let _ = broadcast_tx.send((out_msg.to_string(), Some(client_id_in_recv.clone())));
                            }
                            WsMessage::PanicAction {
                                action,
                                value,
                                ..
                            } => {
                                match action.as_str() {
                                    "blackout" => {
                                        let b = value.unwrap_or(true);
                                        state_manager
                                            .update_param("deck-a", None, None, "blackout", ParamValue::Bool(b))
                                            .await;
                                    }
                                    "freeze" => {
                                        let f = value.unwrap_or(true);
                                        state_manager
                                            .update_param("deck-a", None, None, "freeze", ParamValue::Bool(f))
                                            .await;
                                    }
                                    "test_pattern" => {
                                        let tp = value.unwrap_or(true);
                                        state_manager
                                            .update_param("deck-a", None, None, "test_pattern", ParamValue::Bool(tp))
                                            .await;
                                    }
                                    "fx_kill" => {
                                        state_manager.fx_kill().await;
                                    }
                                    _ => {}
                                }
                                let out_msg = serde_json::json!({
                                    "type": "panic_action",
                                    "action": action,
                                    "value": value,
                                    "client_id": client_id_in_recv,
                                });
                                let _ = broadcast_tx.send((out_msg.to_string(), Some(client_id_in_recv.clone())));
                            }
                            _ => {}
                        }
                    } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                        // MotoVJ 互換フォールバック転送
                        let _ = broadcast_tx.send((val.to_string(), Some(client_id_in_recv.clone())));
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }

        tracing::info!("WebSocket client disconnected: {client_id}");
        send_task.abort();
        broadcast_task.abort();
        audio_task.abort();
    }
}

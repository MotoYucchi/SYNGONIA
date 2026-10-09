//! # SYNGONIA State Engine
//!
//! Single Source of Truth リアルタイム状態管理エンジン。
//!
//! ## 設計原則（SYNGONIA-20261007-DESIGN 準拠）
//! - **Single Source of Truth**: サーバーが唯一の正本状態を保持。
//! - **エコーバック破棄**: `client_id` により、自分が操作した値が跳ね返ってフェーダーが震える現象を完全根絶。
//! - **SnapSync**: iPad 接続時/再接続時に 0.1 秒以内に完全スナップショットを配信。
//! - **Fail-Safe**: ディスク保存はデバウンスで非同期実行し、I/O による描画ブロックをゼロ化。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::RwLock;

/// デッキ ID (deck-a, deck-b)
pub type DeckId = String;

/// レイヤーパラメータ値の型（数値または文字列）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Number(f64),
    Text(String),
    Bool(bool),
}

/// 単一レイヤー定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerDef {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub layer_type: String, // "effect", "video", "generator", etc.
    pub url: String,        // "effects/effect-*.html" など
    pub params: HashMap<String, ParamValue>,
}

/// グループ定義（各エフェクトチャンネル：Opacity, Hue, Mute, Solo 等を束ねる）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDef {
    pub id: String,
    pub name: String,
    pub params: HashMap<String, ParamValue>,
    pub layers: Vec<LayerDef>,
}

/// ビデオ再生状態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoState {
    pub folder_a: String,
    pub track_a: String,
    pub play_a: bool,
    pub speed_a: f64,
    pub time_a: f64,
    pub folder_b: String,
    pub track_b: String,
    pub play_b: bool,
    pub speed_b: f64,
    pub time_b: f64,
    pub crossfader: f64,
    pub opacity_a: f64,
    pub opacity_b: f64,
    pub player_opacity: f64,
    pub player_zindex: String,
}

impl Default for VideoState {
    fn default() -> Self {
        Self {
            folder_a: String::new(),
            track_a: String::new(),
            play_a: false,
            speed_a: 1.0,
            time_a: 0.0,
            folder_b: String::new(),
            track_b: String::new(),
            play_b: false,
            speed_b: 1.0,
            time_b: 0.0,
            crossfader: 0.0,
            opacity_a: 100.0,
            opacity_b: 100.0,
            player_opacity: 100.0,
            player_zindex: "background".to_string(),
        }
    }
}

/// マスターエマージェンシーステート
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterState {
    /// 即座暗転
    pub blackout: bool,
    /// 画面静止
    pub freeze: bool,
    /// 現場疎通テストパターン送出モード
    pub test_pattern: bool,
    /// シェーダーエフェクトモード (0: Test Pattern, 1: Tunnel, 2: Raymarch, 3: Neon Grid, 4: Glitch, 5: 4CH Composite)
    pub effect_mode: i32,
    /// マスタークロスフェーダー (0.0: Deck A - 1.0: Deck B)
    pub master_xfader: f64,
    /// マスターBPM
    pub bpm: f64,
}

impl Default for MasterState {
    fn default() -> Self {
        Self {
            blackout: false,
            freeze: false,
            test_pattern: true, // 起動時は即座に疎通確認できるようテストパターン有効
            effect_mode: 5,     // 4CH Resolume Composite
            master_xfader: 0.0,
            bpm: 128.0,
        }
    }
}

/// SYNGONIA 完全システム状態（Single Source of Truth）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyngoniaState {
    pub decks: HashMap<DeckId, Vec<GroupDef>>,
    pub video: VideoState,
    pub master: MasterState,
    pub seq_no: u64,
}

impl Default for SyngoniaState {
    fn default() -> Self {
        let mut decks = HashMap::new();
        // 初期デフォルトデッキ（MotoVJ 互換チャンネル構造）
        let default_groups = vec![
            GroupDef {
                id: "group-1".to_string(),
                name: "CH 1 (3D Tunnel)".to_string(),
                params: {
                    let mut p = HashMap::new();
                    p.insert("opacity".to_string(), ParamValue::Number(100.0));
                    p.insert("color".to_string(), ParamValue::Text("#00f2fe".to_string()));
                    p.insert("mute".to_string(), ParamValue::Bool(false));
                    p.insert("solo".to_string(), ParamValue::Bool(false));
                    p
                },
                layers: vec![LayerDef {
                    id: "layer-1".to_string(),
                    name: "3D Cyber Tunnel".to_string(),
                    layer_type: "shader".to_string(),
                    url: "shader://cyber-tunnel".to_string(),
                    params: HashMap::new(),
                }],
            },
            GroupDef {
                id: "group-2".to_string(),
                name: "CH 2 (Raymarch)".to_string(),
                params: {
                    let mut p = HashMap::new();
                    p.insert("opacity".to_string(), ParamValue::Number(60.0));
                    p.insert("color".to_string(), ParamValue::Text("#ff007f".to_string()));
                    p.insert("mute".to_string(), ParamValue::Bool(false));
                    p.insert("solo".to_string(), ParamValue::Bool(false));
                    p
                },
                layers: vec![LayerDef {
                    id: "layer-2".to_string(),
                    name: "Audio Raymarch Core".to_string(),
                    layer_type: "shader".to_string(),
                    url: "shader://raymarch-core".to_string(),
                    params: HashMap::new(),
                }],
            },
            GroupDef {
                id: "group-3".to_string(),
                name: "CH 3 (Neon Grid)".to_string(),
                params: {
                    let mut p = HashMap::new();
                    p.insert("opacity".to_string(), ParamValue::Number(30.0));
                    p.insert("color".to_string(), ParamValue::Text("#00ff87".to_string()));
                    p.insert("mute".to_string(), ParamValue::Bool(false));
                    p.insert("solo".to_string(), ParamValue::Bool(false));
                    p
                },
                layers: vec![LayerDef {
                    id: "layer-3".to_string(),
                    name: "Neon Wave Grid".to_string(),
                    layer_type: "shader".to_string(),
                    url: "shader://neon-grid".to_string(),
                    params: HashMap::new(),
                }],
            },
            GroupDef {
                id: "group-4".to_string(),
                name: "CH 4 (Glitch FX)".to_string(),
                params: {
                    let mut p = HashMap::new();
                    p.insert("opacity".to_string(), ParamValue::Number(0.0));
                    p.insert("color".to_string(), ParamValue::Text("#ffd700".to_string()));
                    p.insert("mute".to_string(), ParamValue::Bool(false));
                    p.insert("solo".to_string(), ParamValue::Bool(false));
                    p
                },
                layers: vec![LayerDef {
                    id: "layer-4".to_string(),
                    name: "Acid Glitch Matrix".to_string(),
                    layer_type: "shader".to_string(),
                    url: "shader://acid-glitch".to_string(),
                    params: HashMap::new(),
                }],
            },
        ];

        decks.insert("deck-a".to_string(), default_groups.clone());
        decks.insert("deck-b".to_string(), default_groups);

        Self {
            decks,
            video: VideoState::default(),
            master: MasterState::default(),
            seq_no: 1,
        }
    }
}

/// 16進数カラーコードを RGBA (0.0..1.0) に変換
pub fn parse_hex_color(hex: &str) -> [f32; 4] {
    let clean = hex.trim_start_matches('#');
    if clean.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&clean[0..2], 16),
            u8::from_str_radix(&clean[2..4], 16),
            u8::from_str_radix(&clean[4..6], 16),
        ) {
            return [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
        }
    }
    [1.0, 1.0, 1.0, 1.0]
}

/// レンダースレッド向け 超低遅延パラメータキャッシュ
#[derive(Debug, Clone)]
pub struct RenderCache {
    pub opacities: [f32; 4],
    pub colors: [[f32; 4]; 4],
}

impl Default for RenderCache {
    fn default() -> Self {
        Self {
            opacities: [1.0, 0.5, 0.0, 0.0],
            colors: [
                [0.0, 0.9, 1.0, 1.0], // CH1: Cyan
                [1.0, 0.3, 0.8, 1.0], // CH2: Magenta
                [0.2, 0.9, 0.4, 1.0], // CH3: Green
                [1.0, 0.9, 0.1, 1.0], // CH4: Yellow
            ],
        }
    }
}

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// 状態管理マネージャー
pub struct StateManager {
    state: RwLock<SyngoniaState>,
    storage_dir: PathBuf,
    atomic_blackout: AtomicBool,
    atomic_freeze: AtomicBool,
    atomic_test_pattern: AtomicBool,
    atomic_effect_mode: AtomicI32,
    render_cache: std::sync::RwLock<RenderCache>,
}

impl StateManager {
    pub fn new(storage_dir: impl AsRef<Path>) -> Self {
        let storage_dir = storage_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&storage_dir).ok();

        let initial_state = Self::load_from_disk(&storage_dir).unwrap_or_default();
        let blackout = initial_state.master.blackout;
        let freeze = initial_state.master.freeze;
        let test_pattern = initial_state.master.test_pattern;
        let effect_mode = initial_state.master.effect_mode;

        let sm = Self {
            state: RwLock::new(initial_state),
            storage_dir,
            atomic_blackout: AtomicBool::new(blackout),
            atomic_freeze: AtomicBool::new(freeze),
            atomic_test_pattern: AtomicBool::new(test_pattern),
            atomic_effect_mode: AtomicI32::new(effect_mode),
            render_cache: std::sync::RwLock::new(RenderCache::default()),
        };

        if let Ok(st) = sm.state.try_read() {
            sm.update_render_cache_internal(&st);
        }

        sm
    }

    /// レンダー専用スレッド向け ロックフリー ブラックアウト判定
    pub fn is_blackout(&self) -> bool {
        self.atomic_blackout.load(Ordering::Relaxed)
    }

    /// レンダー専用スレッド向け ロックフリー フリーズ判定
    pub fn is_freeze(&self) -> bool {
        self.atomic_freeze.load(Ordering::Relaxed)
    }

    /// レンダー専用スレッド向け ロックフリー テストパターン判定
    pub fn is_test_pattern(&self) -> bool {
        self.atomic_test_pattern.load(Ordering::Relaxed)
    }

    /// レンダー専用スレッド向け ロックフリー エフェクトモード取得
    pub fn get_effect_mode(&self) -> i32 {
        self.atomic_effect_mode.load(Ordering::Relaxed)
    }

    /// レンダー専用スレッド向け 同期パラメータ取得 (0.001ms 未満)
    pub fn get_render_cache(&self) -> RenderCache {
        self.render_cache.read().unwrap().clone()
    }

    /// 内部キャッシュ更新 (Solo / Mute / Opacity / Color の合成計算)
    fn update_render_cache_internal(&self, state: &SyngoniaState) {
        let mut opacities = [1.0f32, 0.5f32, 0.0f32, 0.0f32];
        let mut colors = [
            [0.0f32, 0.9f32, 1.0f32, 1.0f32],
            [1.0f32, 0.3f32, 0.8f32, 1.0f32],
            [0.2f32, 0.9f32, 0.4f32, 1.0f32],
            [1.0f32, 0.9f32, 0.1f32, 1.0f32],
        ];

        if let Some(groups) = state.decks.get("deck-a") {
            // いずれかのチャンネルで solo が有効かチェック
            let has_solo = groups.iter().take(4).any(|grp| {
                matches!(grp.params.get("solo"), Some(ParamValue::Bool(true)))
            });

            for (i, grp) in groups.iter().take(4).enumerate() {
                let is_solo = matches!(grp.params.get("solo"), Some(ParamValue::Bool(true)));
                let is_mute = matches!(grp.params.get("mute"), Some(ParamValue::Bool(true)));

                if is_mute || (has_solo && !is_solo) {
                    opacities[i] = 0.0;
                } else if let Some(ParamValue::Number(op)) = grp.params.get("opacity") {
                    opacities[i] = (*op as f32 / 100.0).clamp(0.0, 1.0);
                }

                if let Some(ParamValue::Text(hex)) = grp.params.get("color") {
                    colors[i] = parse_hex_color(hex);
                }
            }
        }

        if let Ok(mut cache) = self.render_cache.write() {
            cache.opacities = opacities;
            cache.colors = colors;
        }
    }

    /// 完全スナップショットの取得（SnapSync用）
    pub async fn get_snapshot(&self) -> SyngoniaState {
        self.state.read().await.clone()
    }

    /// パラメータの更新（iPad / PC からの要求）
    pub async fn update_param(
        &self,
        deck_id: &str,
        group_id: Option<&str>,
        layer_id: Option<&str>,
        param_id: &str,
        value: ParamValue,
    ) -> u64 {
        let mut state = self.state.write().await;
        state.seq_no += 1;
        let seq = state.seq_no;

        // ビデオパラメータの更新チェック
        match param_id {
            "crossfader" => {
                if let ParamValue::Number(n) = value {
                    state.video.crossfader = n;
                    state.master.master_xfader = n;
                }
            }
            "blackout" => {
                if let ParamValue::Bool(b) = value {
                    state.master.blackout = b;
                    self.atomic_blackout.store(b, Ordering::Relaxed);
                }
            }
            "freeze" => {
                if let ParamValue::Bool(b) = value {
                    state.master.freeze = b;
                    self.atomic_freeze.store(b, Ordering::Relaxed);
                }
            }
            "test_pattern" => {
                if let ParamValue::Bool(b) = value {
                    state.master.test_pattern = b;
                    self.atomic_test_pattern.store(b, Ordering::Relaxed);
                }
            }
            "effect_mode" => {
                if let ParamValue::Number(m) = value {
                    let mode = m as i32;
                    state.master.effect_mode = mode;
                    self.atomic_effect_mode.store(mode, Ordering::Relaxed);
                }
            }
            "player_opacity" => {
                if let ParamValue::Number(n) = value {
                    state.video.player_opacity = n;
                }
            }
            _ => {}
        }

        // デッキグループ/レイヤーのパラメータ更新
        if let Some(deck) = state.decks.get_mut(deck_id) {
            if let Some(gid) = group_id {
                if let Some(group) = deck.iter_mut().find(|g| g.id == gid) {
                    if let Some(lid) = layer_id {
                        if let Some(layer) = group.layers.iter_mut().find(|l| l.id == lid) {
                            layer.params.insert(param_id.to_string(), value);
                        }
                    } else {
                        // グループパラメータ（opacity, color, mute, solo 等）
                        group.params.insert(param_id.to_string(), value);
                    }
                }
            }
        }

        self.update_render_cache_internal(&state);
        seq
    }

    /// デッキ全体のレイヤー構造一括再構築 (rebuild_layers)
    pub async fn rebuild_layers(&self, deck_id: &str, groups: Vec<GroupDef>) -> u64 {
        let mut state = self.state.write().await;
        state.seq_no += 1;
        state.decks.insert(deck_id.to_string(), groups);
        self.update_render_cache_internal(&state);
        state.seq_no
    }

    /// 緊急アクション：全エフェクト一括消去 (FX Kill)
    pub async fn fx_kill(&self) -> u64 {
        let mut state = self.state.write().await;
        state.seq_no += 1;
        for deck in state.decks.values_mut() {
            for group in deck.iter_mut() {
                group
                    .params
                    .insert("opacity".to_string(), ParamValue::Number(0.0));
            }
        }
        self.update_render_cache_internal(&state);
        state.seq_no
    }

    /// ディスクからの永続化読み込み
    fn load_from_disk(dir: &Path) -> Option<SyngoniaState> {
        let file_path = dir.join("syngonia_state.json");
        if file_path.exists() {
            let data = std::fs::read_to_string(&file_path).ok()?;
            let state: SyngoniaState = serde_json::from_str(&data).ok()?;
            tracing::info!("Loaded SYNGONIA state from disk: {:?}", file_path);
            Some(state)
        } else {
            None
        }
    }

    /// ディスクへの非同期永続化保存
    pub async fn save_to_disk(&self) {
        let state = self.state.read().await.clone();
        let file_path = self.storage_dir.join("syngonia_state.json");

        tokio::task::spawn_blocking(move || {
            if let Ok(json) = serde_json::to_string_pretty(&state) {
                if let Err(e) = std::fs::write(&file_path, json) {
                    tracing::error!("Failed to save state to disk: {e}");
                } else {
                    tracing::info!("State successfully saved to {:?}", file_path);
                }
            }
        });
    }
}

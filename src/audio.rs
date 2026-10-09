//! # SYNGONIA Audio DSP
//!
//! Windows WASAPI Loopback リアルタイムキャプチャ & Flywheel BPM 音声解析エンジン。
//!
//! ## 設計原則（SYNGONIA-20261007-DESIGN 準拠）
//! - **超低遅延**: レイテンシ 10ms 以下で 512 バンド FFT を実行。
//! - **Flywheel BPM**: DJ の曲間や無音時、直前の BPM テンポを自動維持し、映像演出を止めない。
//! - **Fail-Safe**: 音声デバイス切断・無音時もパニックせず自動フォールバック。

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rustfft::{FftPlanner, num_complex::Complex32};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub const FFT_SIZE: usize = 1024;
pub const SPECTRUM_BANDS: usize = 512;

/// 音声解析結果
#[derive(Debug, Clone)]
pub struct AudioAnalysisResult {
    /// 512バンドのマグニチュードスペクトラム (0..255)
    pub spectrum_bytes: Vec<u8>,
    /// 検出されたBPM
    pub bpm: f64,
    /// ビートがヒットしたか
    pub is_beat: bool,
    /// 低域エネルギー (0.0..1.0)
    pub low_energy: f32,
    /// 中域エネルギー (0.0..1.0)
    pub mid_energy: f32,
    /// 高域エネルギー (0.0..1.0)
    pub high_energy: f32,
}

/// リアルタイムレンダラー向け 超低遅延ロックフリー オーディオ指標
#[derive(Debug)]
pub struct LiveAudioMetrics {
    pub low: std::sync::atomic::AtomicU32,  // f32 as u32 bits
    pub mid: std::sync::atomic::AtomicU32,
    pub high: std::sync::atomic::AtomicU32,
    pub beat: std::sync::atomic::AtomicU32,
    pub bpm: std::sync::atomic::AtomicU32,
}

impl Default for LiveAudioMetrics {
    fn default() -> Self {
        Self {
            low: std::sync::atomic::AtomicU32::new(0.0f32.to_bits()),
            mid: std::sync::atomic::AtomicU32::new(0.0f32.to_bits()),
            high: std::sync::atomic::AtomicU32::new(0.0f32.to_bits()),
            beat: std::sync::atomic::AtomicU32::new(0.0f32.to_bits()),
            bpm: std::sync::atomic::AtomicU32::new(128.0f32.to_bits()),
        }
    }
}

impl LiveAudioMetrics {
    pub fn get_low(&self) -> f32 {
        f32::from_bits(self.low.load(Ordering::Relaxed))
    }
    pub fn get_mid(&self) -> f32 {
        f32::from_bits(self.mid.load(Ordering::Relaxed))
    }
    pub fn get_high(&self) -> f32 {
        f32::from_bits(self.high.load(Ordering::Relaxed))
    }
    pub fn get_beat(&self) -> f32 {
        f32::from_bits(self.beat.load(Ordering::Relaxed))
    }
    pub fn get_bpm(&self) -> f32 {
        f32::from_bits(self.bpm.load(Ordering::Relaxed))
    }

    pub fn set_metrics(&self, low: f32, mid: f32, high: f32, beat: f32, bpm: f32) {
        self.low.store(low.to_bits(), Ordering::Relaxed);
        self.mid.store(mid.to_bits(), Ordering::Relaxed);
        self.high.store(high.to_bits(), Ordering::Relaxed);
        self.beat.store(beat.to_bits(), Ordering::Relaxed);
        self.bpm.store(bpm.to_bits(), Ordering::Relaxed);
    }
}

/// オーディオマネージャー
pub struct AudioDspEngine {
    tx: broadcast::Sender<Vec<u8>>,
    is_running: Arc<AtomicBool>,
    flywheel_bpm: Arc<Mutex<f64>>,
    pub metrics: Arc<LiveAudioMetrics>,
}

impl AudioDspEngine {
    pub fn new(broadcast_capacity: usize) -> (Self, broadcast::Receiver<Vec<u8>>) {
        let (tx, rx) = broadcast::channel(broadcast_capacity);
        let is_running = Arc::new(AtomicBool::new(false));
        let flywheel_bpm = Arc::new(Mutex::new(128.0));
        let metrics = Arc::new(LiveAudioMetrics::default());

        let engine = Self {
            tx,
            is_running,
            flywheel_bpm,
            metrics,
        };

        (engine, rx)
    }

    /// オーディオキャプチャストリームの開始
    pub fn start(&self) {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return;
        }

        let is_running = Arc::clone(&self.is_running);
        let tx = self.tx.clone();
        let flywheel_bpm = Arc::clone(&self.flywheel_bpm);
        let metrics = Arc::clone(&self.metrics);

        std::thread::spawn(move || {
            tracing::info!("Initializing WASAPI Loopback Audio Capture...");

            let host = cpal::default_host();
            let device = match host.default_output_device() {
                Some(d) => d,
                None => {
                    tracing::warn!("No default output device found, entering synthetic mode");
                    Self::run_synthetic_loop(is_running, tx, flywheel_bpm, metrics);
                    return;
                }
            };

            let config = match device.default_output_config() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to get default audio config: {e}, entering synthetic mode");
                    Self::run_synthetic_loop(is_running, tx, flywheel_bpm, metrics);
                    return;
                }
            };

            let sample_rate = config.sample_rate().0 as f32;
            let channels = config.channels() as usize;

            let ring_buffer = Arc::new(Mutex::new(Vec::<f32>::with_capacity(FFT_SIZE * 4)));
            let ring_clone = Arc::clone(&ring_buffer);

            let err_fn = |err| tracing::warn!("WASAPI loopback stream error: {err}");

            let stream_result = match config.sample_format() {
                cpal::SampleFormat::F32 => device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _| {
                        if let Ok(mut buf) = ring_clone.lock() {
                            // ステレオ/マルチチャンネルをモノラルにダウンミックス
                            for chunk in data.chunks(channels) {
                                let mono = chunk.iter().sum::<f32>() / channels as f32;
                                buf.push(mono);
                            }
                            if buf.len() > FFT_SIZE * 8 {
                                let excess = buf.len() - FFT_SIZE * 4;
                                buf.drain(0..excess);
                            }
                        }
                    },
                    err_fn,
                    None,
                ),
                _ => {
                    tracing::warn!("Unsupported sample format, entering synthetic mode");
                    Self::run_synthetic_loop(is_running, tx, flywheel_bpm, metrics);
                    return;
                }
            };

            let stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Failed to build input stream: {e}, entering synthetic mode");
                    Self::run_synthetic_loop(is_running, tx, flywheel_bpm, metrics);
                    return;
                }
            };

            if let Err(e) = stream.play() {
                tracing::warn!("Failed to play stream: {e}, entering synthetic mode");
                Self::run_synthetic_loop(is_running, tx, flywheel_bpm, metrics);
                return;
            }

            tracing::info!("WASAPI Loopback stream active! Sample rate: {sample_rate}Hz");

            let mut planner = FftPlanner::new();
            let fft = planner.plan_fft_forward(FFT_SIZE);
            let mut fft_buffer = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
            let mut last_energy = 0.0f32;
            let mut last_beat_time = std::time::Instant::now();
            let mut bpm_history = Vec::<f64>::new();

            while is_running.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(16)); // ~60fps

                let samples = {
                    if let Ok(mut buf) = ring_buffer.lock() {
                        if buf.len() >= FFT_SIZE {
                            buf.drain(0..FFT_SIZE).collect::<Vec<f32>>()
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    }
                };

                if samples.is_empty() {
                    // 音声なし（Flywheel BPM によるシミュレーションパケット送出）
                    let mut spectrum_bytes = vec![0u8; SPECTRUM_BANDS];
                    // 微小なアンビエントノイズ
                    spectrum_bytes[1] = 12;
                    let _ = tx.send(spectrum_bytes);
                    continue;
                }

                // Hanning Window 適用
                for (i, &s) in samples.iter().take(FFT_SIZE).enumerate() {
                    let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos());
                    fft_buffer[i] = Complex32::new(s * w, 0.0);
                }

                fft.process(&mut fft_buffer);

                // 512バンドのマグニチュードスペクトラム算出 (0..255 byte)
                let mut spectrum_bytes = vec![0u8; SPECTRUM_BANDS];
                let mut low_energy = 0.0f32;

                for i in 0..SPECTRUM_BANDS {
                    let c = fft_buffer[i];
                    let mag = (c.re * c.re + c.im * c.im).sqrt() / (FFT_SIZE as f32 / 2.0);
                    let val = (mag * 600.0).clamp(0.0, 255.0) as u8;
                    spectrum_bytes[i] = val;

                    if i < 8 {
                        low_energy += mag;
                    }
                }

                // ビート検出とBPMトラッキング
                let mut is_beat = false;
                if low_energy > last_energy * 1.35 && low_energy > 0.02 {
                    let elapsed = last_beat_time.elapsed().as_secs_f64();
                    if (0.3..=1.0).contains(&elapsed) {
                        is_beat = true;
                        let instant_bpm = 60.0 / elapsed;
                        bpm_history.push(instant_bpm);
                        if bpm_history.len() > 8 {
                            bpm_history.remove(0);
                        }
                        let avg_bpm = bpm_history.iter().sum::<f64>() / bpm_history.len() as f64;
                        if let Ok(mut fw) = flywheel_bpm.lock() {
                            *fw = avg_bpm;
                        }
                        last_beat_time = std::time::Instant::now();
                    }
                }
                last_energy = low_energy * 0.7 + last_energy * 0.3;

                // GPU シェーダー向け ロックフリー指標を更新
                let current_bpm = *flywheel_bpm.lock().unwrap_or_else(|e| e.into_inner());
                let mid_energy = (spectrum_bytes[15..35].iter().map(|&v| v as f32).sum::<f32>() / (20.0 * 255.0)).clamp(0.0, 1.0);
                let high_energy = (spectrum_bytes[80..130].iter().map(|&v| v as f32).sum::<f32>() / (50.0 * 255.0)).clamp(0.0, 1.0);
                let norm_low = (low_energy * 2.5).clamp(0.0, 1.0);
                let beat_val = if is_beat { 1.0 } else { 0.0 };
                metrics.set_metrics(norm_low, mid_energy, high_energy, beat_val, current_bpm as f32);

                // 全クライアントへ512bandバイナリ配信
                let _ = tx.send(spectrum_bytes);
            }
        });
    }

    /// 音声デバイスが未接続またはエラー時の合成フォールバックループ
    fn run_synthetic_loop(
        is_running: Arc<AtomicBool>,
        tx: broadcast::Sender<Vec<u8>>,
        flywheel_bpm: Arc<Mutex<f64>>,
        metrics: Arc<LiveAudioMetrics>,
    ) {
        std::thread::spawn(move || {
            let mut phase = 0.0f32;
            while is_running.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(16));
                phase += 0.08;

                let current_bpm = *flywheel_bpm.lock().unwrap_or_else(|e| e.into_inner());
                let beat_pulse = ((phase * (current_bpm as f32 / 120.0)).sin()).abs();

                let mut spectrum_bytes = vec![0u8; SPECTRUM_BANDS];
                for i in 0..SPECTRUM_BANDS {
                    let freq_decay = (1.0 - (i as f32 / SPECTRUM_BANDS as f32)).powi(2);
                    let val = (beat_pulse * freq_decay * 255.0 * 0.8) as u8;
                    spectrum_bytes[i] = val;
                }

                // 合成モードの指標更新
                let low = beat_pulse * 0.8;
                let mid = (phase * 1.5).sin().abs() * 0.6;
                let high = (phase * 3.0).cos().abs() * 0.5;
                metrics.set_metrics(low, mid, high, beat_pulse, current_bpm as f32);

                let _ = tx.send(spectrum_bytes);
            }
        });
    }

    /// 現在のFlywheel BPMを取得
    pub fn current_bpm(&self) -> f64 {
        *self.flywheel_bpm.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}

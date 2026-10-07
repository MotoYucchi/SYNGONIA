//! # SYNGONIA Network Utilities
//!
//! Hotspot IP 検出 & iPad 接続用 URL 生成

use std::net::UdpSocket;

#[derive(Debug, Clone)]
pub struct NetworkEndpoint {
    pub ip: String,
    pub port: u16,
    pub is_hotspot: bool,
    pub touch_url: String,
    pub pc_url: String,
    pub viewer_url: String,
}

pub struct NetworkManager;

impl NetworkManager {
    /// 利用可能なローカルIPアドレスの検出
    pub fn get_endpoints(port: u16) -> Vec<NetworkEndpoint> {
        let mut endpoints = Vec::new();

        // 1. Windows デフォルト Hotspot IP (192.168.137.1) の存在確認
        if let Ok(socket) = UdpSocket::bind("192.168.137.1:0") {
            let _ = socket;
            endpoints.push(Self::make_endpoint("192.168.137.1", port, true));
        }

        // 2. アクティブなローカルIPの検出（UDPソケットによるルーティング調査）
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            if socket.connect("8.8.8.8:80").is_ok() {
                if let Ok(local_addr) = socket.local_addr() {
                    let ip_str = local_addr.ip().to_string();
                    if !endpoints.iter().any(|e| e.ip == ip_str) {
                        endpoints.push(Self::make_endpoint(&ip_str, port, false));
                    }
                }
            }
        }

        // 3. ループバック (localhost)
        if !endpoints.iter().any(|e| e.ip == "127.0.0.1") {
            endpoints.push(Self::make_endpoint("127.0.0.1", port, false));
        }

        endpoints
    }

    fn make_endpoint(ip: &str, port: u16, is_hotspot: bool) -> NetworkEndpoint {
        let base = if port == 80 {
            format!("http://{ip}")
        } else {
            format!("http://{ip}:{port}")
        };

        NetworkEndpoint {
            ip: ip.to_string(),
            port,
            is_hotspot,
            touch_url: format!("{base}/touch"),
            pc_url: format!("{base}/pc"),
            viewer_url: format!("{base}/viewer"),
        }
    }
}

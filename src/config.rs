#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub listen_addr: String,
    pub opencode_base_url: String,
    pub opencode_api_key: Option<String>,
    pub max_retries: u32,
    pub warp_reset_delay_ms: u64,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:8080".to_string(),
            opencode_base_url: "http://localhost:3000".to_string(),
            opencode_api_key: None,
            max_retries: 3,
            warp_reset_delay_ms: 5000,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppConfig {
    pub listen: String,
    pub upstream: String,
    pub max_retries: u32,
    pub warp_delay: u64,
    pub api_key: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".to_string(),
            upstream: "http://localhost:3000".to_string(),
            max_retries: 3,
            warp_delay: 5000,
            api_key: None,
        }
    }
}

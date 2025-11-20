use serde::{Deserialize, Serialize};
use config::{Config, File, ConfigError};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct StreamConfig {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SrsConfig {
    pub api_url: String,
    pub playback_url_template: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub port: u16,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub srs: SrsConfig,
    pub streams: Vec<StreamConfig>,
    #[serde(default)]
    pub api_keys: Vec<String>,
}

impl AppConfig {
    pub fn new() -> Result<Self, ConfigError> {
        let s = Config::builder()
            .add_source(File::with_name("config"))
            .build()?;

        s.try_deserialize()
    }
}

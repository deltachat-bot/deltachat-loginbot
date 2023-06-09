use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct BotConfig {
    pub email: String,
    pub password: String,
    pub deltachat_db: String,
    pub oauth_db: String,
    pub listen_addr: String,
    pub oauth: OAuthConfig,
    pub enable_request_logging: Option<bool>,
    pub static_dir: Option<String>,
    pub log_level: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

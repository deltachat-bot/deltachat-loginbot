use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct BotConfig {
    pub email: String,
    pub password: String,
    pub deltachat_db: String,
    pub oauth_db: String,
    pub listen_addr: String,
    pub oauth: OAuthConfig,
    pub static_dir: Option<String>,
    pub log_level: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

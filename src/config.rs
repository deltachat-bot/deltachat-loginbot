use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct BotConfig {
    pub email: String,
    pub password: String,
    pub deltachat_db: String,
    pub oauth_db: String,
    pub listen_addr: String,
    pub oauth: OAuthConfig,
    pub enable_request_logging: Option<bool>,
}

#[derive(Deserialize, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct BotConfig {
    pub email: String,
    pub password: String,
    pub deltachat_db: String,
    pub oauth_db: String,
    pub db: String,
    pub listen_addr: String,
    pub oauth: OAuthConfig,
}

#[derive(Deserialize, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

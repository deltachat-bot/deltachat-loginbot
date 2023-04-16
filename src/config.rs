use serde::Deserialize;

#[derive(Deserialize)]
pub struct BotConfig {
    pub email: String,
    pub password: String,
    pub deltachat_db: String,
    pub oauth_db: String,
    pub oauth: OAuthConfig,
}

#[derive(Deserialize)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

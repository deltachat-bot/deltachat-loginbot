use serde::Deserialize;

#[derive(Deserialize)]
pub struct AuthorizeQuery {
    pub client_id: String,
    pub redirect_uri: String,
    pub state: String,
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub code: Option<String>,
}

#![warn(
    unused,
    clippy::correctness,
    missing_debug_implementations,
    clippy::all,
    clippy::indexing_slicing,
    clippy::wildcard_imports,
    clippy::needless_borrow,
    clippy::cast_lossless,
    clippy::unused_async
)]

use serde::Deserialize;

use anyhow::{Context as _, Error};
use deltachat::chat::{create_group, get_chat_contacts, send_msg, ChatId};
use deltachat::contact::{Contact, ContactId};
use deltachat::context::Context;
use deltachat::message::{Message, Viewtype};
use deltachat::qr_code_generator::get_securejoin_qr_svg;
use deltachat::securejoin::get_securejoin_qr;
use serde_json::{json, Value};

use axum::{
    body::Bytes,
    extract::{Form, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, head, post},
    Json, Router,
};
use axum_extra::{
    headers::{authorization::Basic, Authorization, ContentType},
    TypedHeader,
};
use mime::Mime;
use tower::ServiceBuilder;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tower_sessions::{MemoryStore, Session, SessionManagerLayer};

pub use deltachat;

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

#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    pub client_id: String,
    pub redirect_uri: String,
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    pub code: Option<String>,
}


// Short expiry: no logout button, so reuse would skip the QR scan.
const SESSION_EXPIRY_IN_SECONDS: u64 = 15 * 60;

#[derive(Clone, Debug)]
pub struct AppState {
    pub db: sled::Db,
    pub dc_context: Context,
    pub config: BotConfig,
    pub login_html: String,
}

struct AppError(Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        log::error!("internal error: {:#}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong").into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

pub fn build_router(state: AppState, static_dir: String) -> Router {
    let store = MemoryStore::default();
    let session_layer =
        SessionManagerLayer::new(store).with_expiry(tower_sessions::Expiry::OnInactivity(
            time::Duration::seconds(SESSION_EXPIRY_IN_SECONDS as i64),
        ));
    Router::new()
        // OAuth2 authorize: shows login page or redirects if already authenticated
        .route("/authorize", get(authorize_fn))
        // OAuth2 token exchange: validates auth code and returns user info
        .route("/token", post(token_fn))
        .route("/webhook", post(webhook_fn))
        // Creates a DC group and returns the securejoin invite link
        .route("/requestQr", get(requestqr_fn))
        // Returns the invite QR as SVG; HEAD checks if a group exists
        .route("/requestQrSvg", get(requestqr_svg_fn))
        .route("/requestQrSvg", head(requestqr_svg_check_fn))
        // Polled by the login page to detect when the user joined the group
        .route("/checkStatus", get(check_status_fn))
        .nest_service("/", ServeDir::new(static_dir))
        .with_state(state)
        .layer(ServiceBuilder::new().layer(TraceLayer::new_for_http()))
        .layer(session_layer)
}

async fn requestqr_fn(
    State(state): State<AppState>,
    session: Session,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let group = {
        if let Some(group_id) = session.get::<u32>("group_id").await? {
            ChatId::new(group_id)
        } else {
            let mut uuid = uuid::Uuid::new_v4().simple().to_string();
            uuid.truncate(5);
            let group_name = format!("LoginBot group {uuid}");
            let group = create_group(&state.dc_context, &group_name).await?;
            session.insert("group_id", group.to_u32()).await?;
            group
        }
    };
    Ok((
        StatusCode::OK,
        Json(json!({ "link": get_securejoin_qr(&state.dc_context, Some(group)).await? })),
    ))
}

async fn requestqr_svg_check_fn(session: Session) -> StatusCode {
    if session
        .get::<u32>("group_id")
        .await
        .ok()
        .flatten()
        .is_some()
    {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    }
}

async fn requestqr_svg_fn(
    State(state): State<AppState>,
    session: Session,
) -> Result<(StatusCode, TypedHeader<ContentType>, Bytes), AppError> {
    if let Some(group_id) = session.get::<u32>("group_id").await? {
        let qr = get_securejoin_qr_svg(&state.dc_context, Some(ChatId::new(group_id))).await?;
        Ok((
            StatusCode::OK,
            TypedHeader(ContentType::from("image/svg+xml".parse::<Mime>()?)),
            Bytes::from(qr),
        ))
    } else {
        Ok((
            StatusCode::BAD_REQUEST,
            TypedHeader(ContentType::text()),
            Bytes::new(),
        ))
    }
}

async fn check_status_fn(
    State(state): State<AppState>,
    session: Session,
) -> Result<(StatusCode, Json<Value>), AppError> {
    if let Some(group_id) = session.get::<u32>("group_id").await? {
        let dc_context = &state.dc_context;
        log::info!("/checkStatus Getting chat members for group {group_id}");
        let chat_members = get_chat_contacts(dc_context, ChatId::new(group_id)).await?;
        match chat_members.len() {
            1 => Ok((StatusCode::OK, Json(json!({ "waiting": true })))),
            2 => {
                let member_id = chat_members
                    .into_iter()
                    .find(|&c| c != deltachat::contact::ContactId::SELF)
                    .context("could not find user member")?
                    .to_u32();

                if !session.get::<bool>("sent").await?.unwrap_or(false) {
                    let mut msg = Message::new(Viewtype::Text);
                    msg.set_text("This chat is a vehicle to connect you with me, the loginbot. You can leave this chat and delete it now.".to_string());
                    send_msg(dc_context, ChatId::new(group_id), &mut msg).await?;
                    session.insert("contact_id", member_id).await?;
                    session.insert("sent", true).await?;
                }
                Ok((StatusCode::OK, Json(json!({ "success": true }))))
            }
            number_of_members => {
                log::error!("/checkStatus This must not happen. There is/are {number_of_members} in the group {group_id}");
                Err(AppError(Error::msg(format!(
                    "Error! number of chat member {group_id} is not 1 or 2"
                ))))
            }
        }
    } else {
        Ok((
            StatusCode::OK,
            Json(
                json!({ "error": "you need to start the login process first, via /requestQr".to_owned()}),
            ),
        ))
    }
}

#[allow(clippy::unused_async)]
async fn webhook_fn() -> &'static str {
    "ola"
}

async fn authorize_fn(
    Query(queries): Query<AuthorizeQuery>,
    State(state): State<AppState>,
    session: Session,
) -> Result<Response, AppError> {
    let config = &state.config;
    if queries.client_id != config.oauth.client_id {
        log::info!("/authorize Invalid client_id: {}", queries.client_id);
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    if queries.redirect_uri != config.oauth.redirect_uri {
        log::info!("/authorize Invalid redirect_uri: {}", queries.redirect_uri);
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    let auth_code: String = uuid::Uuid::new_v4().simple().to_string();
    let tree = state.db.open_tree("default")?;
    if let Some(contact_id) = session.get::<u32>("contact_id").await? {
        tree.insert(&auth_code, &contact_id.to_le_bytes())?;
        log::info!("/authorize Redirected. Removing contact_id from session");
        session.remove::<u32>("contact_id").await?;
        
        let mut url = url::Url::parse(&queries.redirect_uri).context("invalid redirect uri")?;
        url.query_pairs_mut()
            .append_pair("state", &queries.state)
            .append_pair("code", &auth_code);

        Ok(Redirect::temporary(url.as_str()).into_response())
    } else {
        log::info!("/authorize showing login screen");
        Ok(Html::from(state.login_html).into_response())
    }
}

async fn token_fn(
    State(state): State<AppState>,
    TypedHeader(auth): TypedHeader<Authorization<Basic>>,
    Form(form): Form<TokenQuery>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    if let Some(code) = form.code {
        let client_id: &str = auth.username();
        let client_secret: &str = auth.password();
        if client_id != state.config.oauth.client_id {
            log::info!("/token returned 401 because client_ids were inconsistent");
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(json!( { "error": "incorrect client secret" })),
            ));
        }
        if client_secret != state.config.oauth.client_secret {
            log::info!("/token returned 401 because client_secrets were inconsistent");
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(json!( { "error": "incorrect client secret" })),
            ));
        }
        let tree = state.db.open_tree("default")?;
        log::debug!("/token Opened default tree in sled");
        if let Some(data) = tree.get(code)? {
            let user = Contact::get_by_id(
                &state.dc_context,
                ContactId::new(u32::from_le_bytes(data[..].try_into()?)),
            )
            .await?;
            return Ok((
                StatusCode::OK,
                Json(json!({

                    "access_token": uuid::Uuid::new_v4().to_string(),
                    "token_type": "bearer",
                    "expires_in": 1,
                    "info": {
                        "username": user.get_name(),
                        "email": user.get_addr(),
                    }
                })),
            ));
        }
        log::info!("/token Returning 401 because there is no auth header");
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no auth header"})),
        ));
    }
    log::info!("/token returned 400 because there was not 'code' in queries");
    Ok((
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "no code in form data nor string queries" })),
    ))
}



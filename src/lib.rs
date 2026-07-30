#![recursion_limit = "256"]
#![forbid(unsafe_code)]
#![warn(
    unused,
    clippy::correctness,
    missing_debug_implementations,
    missing_docs,
    clippy::all,
    clippy::wildcard_imports,
    clippy::needless_borrow,
    clippy::cast_lossless,
    clippy::unused_async,
    clippy::explicit_iter_loop,
    clippy::explicit_into_iter_loop,
    clippy::cloned_instead_of_copied,
    clippy::manual_is_variant_and
)]
#![cfg_attr(not(test), warn(clippy::arithmetic_side_effects))]
#![cfg_attr(not(test), forbid(clippy::indexing_slicing))]
#![cfg_attr(not(test), forbid(clippy::string_slice))]
#![allow(
    clippy::match_bool,
    clippy::mixed_read_write_in_expression,
    clippy::bool_assert_comparison,
    clippy::manual_split_once,
    clippy::format_push_string,
    clippy::bool_to_int_with_if
)]
//! Loginbot library: OAuth2 login flow backed by Delta Chat securejoin.
//!
//! Exposes [`build_router`] which wires up all HTTP handlers.

use serde::Deserialize;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::from_utf8;

use anyhow::{Context as _, Error};
use deltachat::chat::{create_group, get_chat_contacts, send_msg, ChatId};
use deltachat::config::get_all_ui_config_keys;
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

/// Top-level configuration read from `config.toml`.
#[derive(Deserialize, Clone, Debug)]
pub struct BotConfig {
    /// Email address the bot account uses for IMAP/SMTP.
    pub email: String,
    /// IMAP/SMTP password for the bot account.
    pub password: String,
    /// Path to the Delta Chat SQLite database file.
    pub deltachat_db: PathBuf,
    /// Path to the sled key-value store used for OAuth codes and identities.
    pub oauth_db: PathBuf,
    /// Socket address the HTTP server listens on (e.g. `"127.0.0.1:8080"`).
    pub listen_addr: SocketAddr,
    /// Shadow login address domain which should never have an MX record.
    pub shadow_domain: String,
    /// OAuth2 client configuration (id, secret, redirect URI).
    pub oauth: OAuthConfig,
    /// Directory from which static files (login.html, …) are served.
    pub static_dir: Option<PathBuf>,
    /// Tracing log level string (e.g. `"info"`). Defaults to `WARN`.
    pub log_level: Option<String>,
}

/// OAuth2 client credentials and callback URI.
#[derive(Deserialize, Clone, Debug)]
pub struct OAuthConfig {
    /// OAuth2 `client_id` that must match the value sent by the relying party.
    pub client_id: String,
    /// Shared secret used to authenticate the relying party on `/token`.
    pub client_secret: String,
    /// Redirect URI the bot will forward the auth code to.
    pub redirect_uri: String,
}

/// Query parameters expected on the `/authorize` endpoint.
#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    /// OAuth2 client identifier.
    pub client_id: String,
    /// URI the bot redirects to after successful authentication.
    pub redirect_uri: String,
    /// Opaque state value echoed back to the relying party.
    pub state: String,
}

/// Form/query parameters expected on the `/token` endpoint.
#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    /// The one-time authorization code issued by `/authorize`.
    pub code: Option<String>,
}

// Short expiry: no logout button, so reuse would skip the QR scan.
const SESSION_EXPIRY_IN_SECONDS: u64 = 15 * 60;

/// Config key prefix of the legacy map, followed by a fingerprint.
///
/// The map lives in the account, where it cannot be separated from the
/// contacts it is about. `ui.` is core's namespace for keys of its API
/// users.
const LEGACY_PREFIX: &str = "ui.loginbot.addr.";

/// Config key marking the legacy map as sealed.
const SEAL_KEY: &str = "ui.loginbot.legacy_sealed";

/// Sled tree legacy deployments hold, mapping a fingerprint to the
/// address it was handed out as. Read only once when filling the legacy map.
const IDENTITIES_TREE: &str = "identities";

/// Refusal shown when a contact cannot be a login identity.
const NO_KEY: &str = "login requires a verified Delta Chat profile";

/// Shared state cloned into every Axum handler.
#[derive(Clone, Debug)]
pub struct AppState {
    /// Sled database holding the OAuth codes.
    pub db: sled::Db,
    /// Delta Chat context running the bot account.
    pub dc_context: Context,
    /// Parsed configuration from `config.toml`.
    pub config: BotConfig,
    /// Contents of `login.html`, served when a user needs to scan the QR.
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

/// Returns a contact and its login identity, the lowercase hex of its
/// OpenPGP fingerprint, or `None` if it must not log in.
async fn login_identity(
    context: &Context,
    contact_id: ContactId,
) -> Result<Option<(Contact, String)>, Error> {
    let contact = Contact::get_by_id(context, contact_id).await?;
    let Some(fingerprint) = contact.fingerprint() else {
        return Ok(None);
    };
    if !contact.is_verified(context).await? {
        return Ok(None);
    }
    Ok(Some((contact, fingerprint.hex().to_lowercase())))
}

/// Returns the email address the relying party uses as an identity.
///
/// Users contained in the legacy map keep the address their account was
/// originally keyed on, everybody else is named by their key.
async fn login_addr(
    context: &Context,
    shadow_domain: &str,
    fingerprint: &str,
) -> Result<String, Error> {
    match context.get_ui_config(&legacy_key(fingerprint)).await? {
        Some(addr) => Ok(addr),
        None => Ok(format!("{fingerprint}@{shadow_domain}")),
    }
}

/// Returns the legacy map key of `fingerprint`.
fn legacy_key(fingerprint: &str) -> String {
    format!("{LEGACY_PREFIX}{}", fingerprint.to_lowercase())
}

/// Returns how many addresses the legacy map holds.
async fn legacy_map_len(context: &Context) -> Result<usize, Error> {
    let keys = get_all_ui_config_keys(context).await?;
    Ok(keys
        .iter()
        .filter(|key| key.starts_with(LEGACY_PREFIX))
        .count())
}

/// Puts `addr` into the legacy map unless the key is already mapped.
async fn record_legacy_addr(context: &Context, fingerprint: &str, addr: &str) -> Result<(), Error> {
    let key = legacy_key(fingerprint);
    if addr.contains('@') && context.get_ui_config(&key).await?.is_none() {
        context.set_ui_config(&key, Some(addr)).await?;
    }
    Ok(())
}

/// Permanently seal the legacy use of external email addresses as identity,
/// now that loginbot uses key-based shadow addresses.
///
/// Can be called multiple times, but will only seal once on first call.
///
/// Returns how many addresses the map holds.
pub async fn seal_legacy_map(context: &Context, db: &sled::Db) -> Result<usize, Error> {
    if context.get_ui_config(SEAL_KEY).await?.is_some() {
        return legacy_map_len(context).await;
    }
    // What `identities` holds was handed out, what a contact holds was
    // last seen, so `identities` wins where both know a key.
    for item in &db.open_tree(IDENTITIES_TREE)? {
        let (fingerprint, addr) = item?;
        record_legacy_addr(context, from_utf8(&fingerprint)?, from_utf8(&addr)?).await?;
    }
    // `get_all` with no flags lists key-contacts only.
    for contact_id in Contact::get_all(context, 0, None).await? {
        let contact = Contact::get_by_id(context, contact_id).await?;
        if let Some(fingerprint) = contact.fingerprint() {
            record_legacy_addr(context, &fingerprint.hex(), contact.get_addr()).await?;
        }
    }
    context.set_ui_config(SEAL_KEY, Some("1")).await?;
    legacy_map_len(context).await
}

/// Build the Axum [`Router`] with all loginbot HTTP handlers attached.
///
/// `static_dir` is the filesystem path served at `/` for static assets.
pub fn build_router(state: AppState, static_dir: PathBuf) -> Router {
    let store = MemoryStore::default();
    let session_layer =
        SessionManagerLayer::new(store).with_expiry(tower_sessions::Expiry::OnInactivity(
            time::Duration::seconds(SESSION_EXPIRY_IN_SECONDS as i64),
        ));
    Router::new()
        // OAuth2 authorize: shows login page or redirects if already authenticated
        .route("/authorize", get(get_authorize))
        // OAuth2 token exchange: validates auth code and returns user info
        .route("/token", post(post_token))
        // Creates a DC group and returns the securejoin invite link
        .route("/requestQr", get(get_requestqr))
        // Returns the invite QR as SVG; HEAD checks if a group exists
        .route("/requestQrSvg", get(get_requestqr_svg))
        .route("/requestQrSvg", head(head_requestqr_svg))
        // Polled by the login page to detect when the user joined the group
        .route("/checkStatus", get(get_checkstatus))
        .nest_service("/", ServeDir::new(static_dir))
        .with_state(state)
        .layer(ServiceBuilder::new().layer(TraceLayer::new_for_http()))
        .layer(session_layer)
}

async fn get_requestqr(
    State(state): State<AppState>,
    session: Session,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let mut uuid = uuid::Uuid::new_v4().simple().to_string();
    uuid.truncate(5);
    let group = create_group(&state.dc_context, &format!("LoginBot group {uuid}")).await?;
    // Reset per-login state so that a second login attempt from the same
    // browser session starts fresh (old `sent` flag must not carry over).
    session.insert("group_id", group.to_u32()).await?;
    session.remove::<bool>("sent").await?;
    session.remove::<u32>("contact_id").await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "link": get_securejoin_qr(&state.dc_context, Some(group)).await? })),
    ))
}

async fn head_requestqr_svg(session: Session) -> StatusCode {
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

async fn get_requestqr_svg(
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

async fn get_checkstatus(
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
                    let Some((_, fingerprint)) =
                        login_identity(dc_context, ContactId::new(member_id)).await?
                    else {
                        log::warn!("/checkStatus Refusing contact {member_id}, no verified key.");
                        return Ok((StatusCode::FORBIDDEN, Json(json!({ "error": NO_KEY }))));
                    };
                    log::info!("/checkStatus Contact {member_id} authenticated as {fingerprint}.");

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

async fn get_authorize(
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
        log::info!("/authorize Redirected. Clearing session state.");
        // Flush the whole session so the next login starts completely fresh.
        // (group_id and sent must not carry over to a second login attempt.)
        session.flush().await?;

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

async fn post_token(
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
            let bytes = <[u8; 4]>::try_from(&*data)?;
            let contact_id = ContactId::new(u32::from_le_bytes(bytes));
            let Some((contact, fingerprint)) =
                login_identity(&state.dc_context, contact_id).await?
            else {
                log::warn!("/token Refusing contact {contact_id}, no verified key.");
                return Ok((StatusCode::FORBIDDEN, Json(json!({ "error": NO_KEY }))));
            };
            let addr =
                login_addr(&state.dc_context, &state.config.shadow_domain, &fingerprint).await?;
            log::info!("/token Key {fingerprint} logs in as {addr}.");
            return Ok((
                StatusCode::OK,
                Json(json!({
                    "access_token": uuid::Uuid::new_v4().to_string(),
                    "token_type": "bearer",
                    "expires_in": 1,
                    "info": {
                        "id": fingerprint,
                        "username": contact.get_name(),
                        "email": addr,
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

#[cfg(test)]
mod tests {
    use super::*;
    use deltachat::context::ContextBuilder;

    const FP: &str = "9d1c2e3f4a5b6c7d8e9f0a1b2c3d4e5f60718293";
    const DOMAIN: &str = "loginbot.testrun.org";

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_shadow_addr_unless_in_legacy_map() -> Result<(), Error> {
        let dir = tempfile::tempdir()?;
        let context = ContextBuilder::new(dir.path().join("dc.db")).open().await?;
        assert_eq!(
            login_addr(&context, DOMAIN, FP).await?,
            format!("{FP}@{DOMAIN}")
        );

        // Fingerprints are mapped lowercase, whatever case they come in.
        record_legacy_addr(&context, &FP.to_uppercase(), "user@example.org").await?;
        assert_eq!(login_addr(&context, DOMAIN, FP).await?, "user@example.org");

        // A mapped address is never replaced.
        record_legacy_addr(&context, FP, "other@example.org").await?;
        assert_eq!(login_addr(&context, DOMAIN, FP).await?, "user@example.org");
        assert_eq!(legacy_map_len(&context).await?, 1);
        Ok(())
    }
}

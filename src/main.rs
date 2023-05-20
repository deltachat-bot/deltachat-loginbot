mod config;
mod queries;

use std::env::{args, current_dir};
use std::fs::read;
use std::path::{Path, PathBuf};
use std::str::from_utf8;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context as _, Error, bail};
use base64::Engine;
use deltachat::chat::{create_group_chat, get_chat_contacts, ChatId, ProtectionStatus, send_msg};
use deltachat::config::Config;
use deltachat::contact::{Contact, ContactId};
use deltachat::context::{Context, ContextBuilder};
use deltachat::securejoin::get_securejoin_qr;
use deltachat::qr_code_generator::get_securejoin_qr_svg;
use deltachat::message::{Message, Viewtype};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use axum::{
    routing::{get, post, head},
    http::StatusCode,
    body::Bytes,
    extract::{Query, State, Form},
    Router,
    response::{IntoResponse, Response, Redirect, Html},
    extract::TypedHeader,
    headers::{ContentType, Authorization, authorization::Basic},
    Json,
};
use tower_http::services::ServeDir;
use axum_sessions::{
    async_session::MemoryStore,
    extractors::{ReadableSession, WritableSession},
    SessionLayer,
};
use mime::Mime;

use crate::config::BotConfig;
use crate::queries::*;

// Short expiry is important, because right now we don't have an logout button on the login page
// And even if we did have one, users would never get the idea that they not only need to logout of discourse,
// but also out of the login with DC service.
// Otherwise one click without scanning another qr code would log them in again as long as the session is valid.
// Which can be bad in a "shared public library computer" situation.
//
// Session cookies might be ideal,
// but according to https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Set-Cookie#attributes
// many browsers allow to restore them when resuming the browsing session or recovering it.
const SESSION_EXPIRY_IN_SECONDS: u64 = 15 * 60;

#[derive(Clone, Debug)]
struct AppState {
    db: sled::Db,
    dc_context: Context,
    config: BotConfig,
}

struct AppError(Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong").into_response()
    }
}

impl<E> From<E> for AppError
where E: Into<Error> {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let botconfig: BotConfig;
    {
        let mut config_file_path = current_dir()
            .expect("Cannot get current directory")
            .join("config.toml");
        if let Some(arg) = args().nth(1) {
            config_file_path = PathBuf::from(arg);
        }
        botconfig = toml::from_str(from_utf8(&read(config_file_path)?)?)?;
    }
    let level: String = botconfig.log_level.clone().unwrap_or("warn".to_string());
    if let Ok(level) = femme::LevelFilter::from_str(&level) {
        femme::with_level(level);
        println!("Starting logging with {level} level");
    } else {
        femme::with_level(femme::LevelFilter::Warn);
        println!("No log level provided, thus logging with WARN level");
    }
    //log::info!("Starting the bot. Address: {}", botconfig.email);
    //log::info!("Open bot db");
    let db = sled::open(&botconfig.oauth_db)?;
    //log::info!("Open deltachat context");
    let ctx = ContextBuilder::new(botconfig.deltachat_db.clone().into())
        .open()
        .await
        .context("Creating context failed")?;
    /*
    let dc_events = ctx.get_event_emitter();
    let dc_event_task = tokio::spawn(async move {
        while let Some(event) = dc_events.recv().await {
            use deltachat::EventType;
            match event.typ {
                EventType::Error(message) => log::error!("{}", message),
                EventType::Warning(message) => log::warn!("{}", message),
                EventType::Info(message) => log::info!("{}", message),
                event => log::debug!("{:?}", event),
            }
        }
    });
    */
    let state: AppState = AppState {
        db,
        dc_context: ctx.clone(),
        config: botconfig.clone(),
    };
    /*
    if botconfig.enable_request_logging == Some(true) {
        backend.with(tide::log::LogMiddleware::new());
    }
    */
    let secret: [u8; 128] = {
        let mut secret = [0u8; 128];
        let mut rng = rand::rngs::OsRng::default();
        rng.fill_bytes(&mut secret);
        secret
    };
    let store = MemoryStore::new();
    let session_layer = SessionLayer::new(store, &secret);
    let backend = Router::new()
        .route("/", get(index))
        // This endpoint is there only for debugging the web API. Like if there is connection to it or
        // not.
        //.route("/authorize", get(authorize_fn))
        // Authorize API which is called the first time and shows the login screen
        // If the authorization is done, this API redirects to the specified redirect URI specified in
        // the web APIs config(e.g. the discouse callback API) rather than showing the llogin screen.
        .route("/token", post(token_fn))
        // Token API is called by the OAuth2(e.g. Discourse) to see if the user
        // has been authenticated by us.
        .route("/webhook", post(webhook_fn))
        // This always returns 200 OK. Maybe remove this if it's not required?
        .route("/requestQr", get(requestqr_fn))
        // This is the first API called in the /authorize login page. Creates a group and sends back
        // the invite link
        .route("/requestQrSvg", get(requestqr_svg_fn))
        // This must be called after /requestQr and is for the SVG generated by DeltaChat for joining
        // the group. /requestQr just returns the openpgp link but this returns a QR in SVG format
        // which user can scan from their phone or something to join the group.
        .route("/requestQrSvg", head(requestqr_svg_check_fn))
        // This is same as above API but lighter just for checking purposes.
        .route("/checkStatus", get(check_status_fn))
        // This is called on an interval(5s?) by the /authorize login page to see if the user has
        // joined the group created by /requestQr which means they have authenticated with their email
        // address.
        .nest_service("/", ServeDir::new(botconfig.static_dir.unwrap_or("./static".to_string())))
        // This is for static files. See the function to see a list of files.
        .with_state(state)
        .layer(session_layer);


    if !ctx.get_config_bool(Config::Configured).await? {
        //log::info!("Configure deltachat context");
        ctx.set_config(Config::Addr, Some(botconfig.email.clone().as_str()))
            .await?;
        ctx.set_config(Config::MailPw, Some(botconfig.password.clone().as_str()))
            .await?;
        ctx.set_config(Config::Bot, Some("1")).await?;
        ctx.set_config(Config::E2eeEnabled, Some("1")).await?;
        ctx.configure().await.context("configuration failed...")?;
    }
    // connect to email server
    //log::info!("Serving static files from {}", botconfig.static_dir.unwrap_or("./static/".to_string()));
    ctx.start_io().await;
    axum::Server::bind(&botconfig.listen_addr.parse()?).serve(backend.into_make_service()).await?;
    //backend.listen(botconfig.listen_addr.clone()).await?;
    tokio::signal::ctrl_c().await?;
    //log::info!("Shutting Down");
    ctx.stop_io().await;
    //dc_event_task.await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html("A loginbot instance is running here")
}

async fn requestqr_fn(State(state): State<AppState>, mut session: WritableSession) -> Result<(StatusCode, Json<Value>), AppError> {
    let group = {
        if let Some(group_id) = session.get::<u32>("group_id") {
            ChatId::new(group_id)
        } else {
            let mut uuid = uuid::Uuid::new_v4().simple().to_string();
            uuid.truncate(5);
            let group_name = format!("LoginBot group {uuid}");
            let group =
                create_group_chat(&state.dc_context, ProtectionStatus::Protected, &group_name).await?;
            session.insert("group_id", group.to_u32())?;
            group
        }
    };
    Ok((StatusCode::OK, Json(json!({ "link": get_securejoin_qr(&state.dc_context, Some(group)).await? }))))
}

async fn requestqr_svg_check_fn(State(state): State<AppState>, session: ReadableSession) -> StatusCode {
    if session.get::<u32>("group_id").is_some() {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    }
}

async fn requestqr_svg_fn(State(state): State<AppState>, session: ReadableSession) -> Result<(StatusCode, TypedHeader<ContentType>, Bytes), AppError> {
    if let Some(group_id) = session.get::<u32>("group_id") {
        let qr = get_securejoin_qr_svg(&state.dc_context, Some(ChatId::new(group_id))).await?;
        Ok((StatusCode::OK, TypedHeader(ContentType::from("image/svg+xml".parse::<Mime>()?)), Bytes::from(qr)))
    } else {
        Ok((StatusCode::BAD_REQUEST, TypedHeader(ContentType::text()), Bytes::new()))
    }
}

async fn check_status_fn(State(state): State<AppState>, mut session: WritableSession) -> Result<(StatusCode, Json<Value>), AppError> {
    if let Some(group_id) = session.get::<u32>("group_id") {
        let dc_context = &state.dc_context;
        //log::info!("/checkStatus Getting chat members for group {group_id}");
        let chat_members = get_chat_contacts(dc_context, ChatId::new(group_id)).await?;
        match chat_members.len() {
            1 => Ok((StatusCode::OK, Json(json!({ "waiting": true })))),
            2 => {
                let i = {
                    if chat_members[0] == deltachat::contact::ContactId::SELF {
                        1
                    } else {
                        0
                    }
                };
                let mut msg = Message::new(Viewtype::Text);
                msg.set_text(Some("This chat is a vehicle to connect you with me, the loginbot. You can leave this chat and delete it now.".to_string()));
                send_msg(&dc_context, ChatId::new(group_id), &mut msg).await?;
                session.insert("contact_id", chat_members[i].to_u32())?;
                Ok((StatusCode::OK, Json(json!({ "success": true }))))
            }
            _number_of_members => {
                //log::error!("{}", format!("/checkStatus This must not happen. There is/are {number_of_members} in the group {group_id}"));
                Err(AppError(Error::msg(format!("Error! number of chat member {} is not 1 or 2", group_id))))
            }
        }
    } else {
        Ok((StatusCode::OK, Json(json!({ "error": "you need to start the login process first, via /requestQr".to_owned()}))))
    }
}

async fn webhook_fn() -> &'static str {
    ""
}

async fn authorize_fn(Query(queries): Query<AuthorizeQuery>, State(state): State<AppState>, session: ReadableSession) -> Result<Response, AppError> {
    let config = &state.config;
    if queries.client_id != config.oauth.client_id {
        //log::info!("/authorize Invalid client_id: {}", queries.client_id);
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    if queries.redirect_uri != config.oauth.redirect_uri {
        //log::info!("/authorize Invalid redirect_uri: {}", queries.redirect_uri);
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    let auth_code: String = uuid::Uuid::new_v4().simple().to_string();
    let tree = state.db.open_tree("default")?;
    if let Some(contact_id) = session.get::<u32>("contact_id") {
        tree.insert(&auth_code, &contact_id.to_le_bytes())?;
        tree.insert(contact_id.to_le_bytes(), &*auth_code)?;
        // is it really required to save both pairs?
        //log::info!("/authorize Redirected");
        Ok(Redirect::temporary(&format!(
            "{}?state={}&code={auth_code}",
            queries.redirect_uri, queries.state)).into_response())
    } else {
        //log::info!("/authorize showing login screen");
        Ok(Redirect::temporary("/login").into_response())
    }
}

async fn token_fn(State(state): State<AppState>, Query(queries): Query<TokenQuery>, Form(form_input): Form<TokenQuery>, TypedHeader(auth): TypedHeader<Authorization<Basic>>) -> Result<(StatusCode, Json<Value>), AppError> {
    let code: Option<String> = {
        if queries.code.is_none() {
            form_input.code
        } else {
            queries.code
        }
    };
    if let Some(code) = code {
        let client_id: &str = auth.username();
        let client_secret: &str = auth.password();
        if client_id != state.config.oauth.client_id {
            //log::info!("/token returned 401 because client_ids were inconsistent");
            return Ok((StatusCode::UNAUTHORIZED, Json(json!( { "error": "incorrect client secret" }))));
        }
        if client_secret != state.config.oauth.client_secret {
            //log::info!("/token returned 401 because client_secrets were inconsistent");
            return Ok((StatusCode::UNAUTHORIZED, Json(json!( { "error": "incorrect client secret" }))));
        }
        let tree = state.db.open_tree("default")?;
        //log::debug!("/token Opened default tree in sled");
        if let Some(data) = tree.get(code)? {
            let user = Contact::load_from_db(
                &state.dc_context,
                ContactId::new(u32::from_le_bytes(data[..].try_into()?)),
                // this should be in parrallel with the convert in /authorize
                // that, if I expect a u32 in little-endian, it must be saved as such
                // in /authorize as well
            )
            .await?;
            return Ok((StatusCode::OK, Json(json!({ 

                    "access_token": uuid::Uuid::new_v4().to_string(),
                    "token_type": "bearer",
                    "expires_in": 1,
                    "info": {
                        "username": user.get_name(),
                        "email": user.get_addr(),
                    }
                }))));
        }
        //log::info!("/token Returning 401 because there is no auth header");
        return Ok((StatusCode::UNAUTHORIZED, Json(json!({ "error": "no auth header"}))));
    }
    //log::info!("/token returned 400 because there was not 'code' in queries");
    Ok((StatusCode::BAD_REQUEST, Json(json!({ "error": "no code in form data nor string queries" }))))
}

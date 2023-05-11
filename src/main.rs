mod config;
mod queries;

use std::env::{args, current_dir};
use std::fs::read;
use std::path::{Path, PathBuf};
use std::str::from_utf8;
use std::time::Duration;
use std::str::FromStr;

use anyhow::Context as _;
use base64::Engine;
use deltachat::chat::{create_group_chat, get_chat_contacts, ChatId, ProtectionStatus, send_msg};
use deltachat::config::Config;
use deltachat::contact::{Contact, ContactId};
use deltachat::context::{Context, ContextBuilder};
use deltachat::securejoin::get_securejoin_qr;
use deltachat::qr_code_generator::get_securejoin_qr_svg;
use deltachat::message::{Message, Viewtype};
use tide::log;
use tide::prelude::*;
use tide::sessions::{MemoryStore, SessionMiddleware};
use tide::{Body, Redirect, Request, Response};
use rand::RngCore;

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
struct State {
    db: sled::Db,
    dc_context: Context,
    config: BotConfig,
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
    log::info!("Starting the bot. Address: {}", botconfig.email);
    log::info!("Open bot db");
    let db = sled::open(&botconfig.oauth_db)?;
    log::info!("Open deltachat context");
    let ctx = ContextBuilder::new(botconfig.deltachat_db.clone().into())
        .open()
        .await
        .context("Creating context failed")?;
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

    let state = State {
        db,
        dc_context: ctx.clone(),
        config: botconfig.clone(),
    };
    let mut backend = tide::with_state(state);
    if botconfig.enable_request_logging == Some(true) {
        backend.with(tide::log::LogMiddleware::new());
    }
    let secret: [u8; 48] = {
        let mut secret = [0u8; 48];
        let mut rng = rand::rngs::OsRng::default();
        rng.fill_bytes(&mut secret);
        secret
    };
    backend.with(
        SessionMiddleware::new(
            MemoryStore::new(),
            &secret, 
        )
        .with_cookie_name("session")
        .with_session_ttl(Some(Duration::from_secs(SESSION_EXPIRY_IN_SECONDS))),
    );
    // this "secret" must be changed to something random in production
    backend
        .at("/")
        .get(|_| async { Ok("Hello, this is an instance of a 'login with deltachat'-Bot.") });
    // This endpoint is there only for debugging the web API. Like if there is connection to it or
    // not.
    backend.at("/authorize").get(authorize_fn);
    // Authorize API which is called the first time and shows the login screen
    // If the authorization is done, this API redirects to the specified redirect URI specified in
    // the web APIs config(e.g. the discouse callback API) rather than showing the llogin screen.
    backend.at("/token").post(token_fn);
    // Token API is called by the OAuth2(e.g. Discourse) to see if the user
    // has been authenticated by us.
    backend.at("/webhook").post(webhook_fn);
    // This always returns 200 OK. Maybe remove this if it's not required?
    backend.at("/requestQr").get(requestqr_fn);
    // This is the first API called in the /authorize login page. Creates a group and sends back
    // the invite link
    backend.at("/requestQrSvg").get(requestqr_svg_fn);
    // This must be called after /requestQr and is for the SVG generated by DeltaChat for joining
    // the group. /requestQr just returns the openpgp link but this returns a QR in SVG format
    // which user can scan from their phone or something to join the group.
    backend.at("/requestQrSvg").head(requestqr_svg_check_fn);
    // This is same as above API but lighter just for checking purposes.
    backend.at("/checkStatus").get(check_status_fn);
    // This is called on an interval(5s?) by the /authorize login page to see if the user has
    // joined the group created by /requestQr which means they have authenticated with their email
    // address.
    backend.at("/:filename").get(static_file_fn);
    // This is for static files. See the function to see a list of files.

    if !ctx.get_config_bool(Config::Configured).await? {
        log::info!("Configure deltachat context");
        ctx.set_config(Config::Addr, Some(botconfig.email.clone().as_str()))
            .await?;
        ctx.set_config(Config::MailPw, Some(botconfig.password.clone().as_str()))
            .await?;
        ctx.set_config(Config::Bot, Some("1")).await?;
        ctx.set_config(Config::E2eeEnabled, Some("1")).await?;
        ctx.configure().await.context("configuration failed...")?;
    }
    // connect to email server
    log::info!("Serving static files from {}", botconfig.static_dir.unwrap_or("./static/".to_string()));
    ctx.start_io().await;

    backend.listen(botconfig.listen_addr.clone()).await?;
    tokio::signal::ctrl_c().await?;
    log::info!("Shutting Down");
    ctx.stop_io().await;
    dc_event_task.await?;
    Ok(())
}

async fn static_file_fn(req: Request<State>) -> tide::Result {
    let filename = req.param("filename")?;
    match filename {
        "delta-chat-logo.svg" | "favicon.ico" | "login.html" | "styles.css" => {
            Ok(Response::builder(200)
                .body(Body::from_file(Path::new("./static/").join(filename)).await?)
                .build())
        }
        _ => Ok(Response::builder(404).body(Body::empty()).build()),
    }
}

async fn requestqr_fn(mut req: Request<State>) -> tide::Result {
    let mut uuid = uuid::Uuid::new_v4().simple().to_string();
    uuid.truncate(5);
    let group_name = format!("LoginBot group {uuid}");
    let state = req.state();
    // TODO check first if group for the session already exists?
    let group =
        create_group_chat(&state.dc_context, ProtectionStatus::Protected, &group_name).await?;
    let body = Body::from_json(&json!({"link": get_securejoin_qr(&state.dc_context, Some(group)).await?}))?;
    req.session_mut().insert("group_id", group.to_u32())?;
    Ok(Response::builder(200).body(body).build())
}

async fn requestqr_svg_check_fn(req: Request<State>) -> tide::Result {
    if req.session().get::<u32>("group_id").is_some() {
        Ok(Response::builder(200).body(Body::empty()).build())
    } else {
        Ok(Response::builder(400).body(Body::empty()).build())
    }
}

async fn requestqr_svg_fn(req: Request<State>) -> tide::Result {
    if let Some(group_id) = req.session().get::<u32>("group_id") {
        let state = req.state();
        let mut body = Body::from_string(get_securejoin_qr_svg(&state.dc_context, Some(ChatId::new(group_id))).await?);
        body.set_mime("image/svg+xml");
        Ok(Response::builder(200).body(body).build())
    } else {
        Ok(Response::builder(400).body(Body::empty()).build())
    }
}

async fn check_status_fn(mut req: Request<State>) -> tide::Result {
    if let Some(group_id) = req.session().get::<u32>("group_id") {
        let dc_context = &req.state().dc_context;
        log::info!("/checkStatus Getting chat members for group {group_id}");
        let chat_members = get_chat_contacts(dc_context, ChatId::new(group_id)).await?;
        match chat_members.len() {
            1 => Ok(Response::builder(200)
                .body(Body::from_json(&json!({"waiting": true}))?)
                .build()),
            2 => {
                let i = {
                    if chat_members[0] == deltachat::contact::ContactId::SELF {
                        1
                    } else {
                        0
                    }
                };
                /*
                let mut msg = Message::new(Viewtype::Text);
                msg.set_text(Some("This chat is a vehicle to connect you with me, the loginbot. You can leave this chat and delete it now.".to_string()));
                send_msg(dc_context, ChatId::new(group_id), &mut msg).await?;
                */
                req.session_mut()
                    .insert("contact_id", chat_members[i].to_u32())?;

                Ok(Response::builder(200)
                    .body(Body::from_json(&json!({"success": true}))?)
                    .build())
            }
            number_of_members => {
                log::error!("{}", format!("/checkStatus This must not happen. There is/are {number_of_members} in the group {group_id}"));
                Err(tide::Error::from_str(500, "Some internal error occured..."))
            }
        }
    } else {
        Ok(Response::builder(401)
            .body(Body::from_json(
                &json!({"error": "you need to start the login process first, via /requestQR".to_owned()}),
            )?)
            .build())
    }
}

async fn webhook_fn(_req: Request<State>) -> tide::Result {
    Ok(Response::builder(200).body(Body::empty()).build())
}

async fn authorize_fn(req: Request<State>) -> tide::Result {
    let queries: AuthorizeQuery = req.query()?;
    let state = req.state();
    let config = &state.config;
    if queries.client_id != config.oauth.client_id {
        log::info!("/authorize Invalid client_id: {}", queries.client_id);
        return Ok(Response::builder(400).build());
    }
    if queries.redirect_uri != config.oauth.redirect_uri {
        log::info!("/authorize Invalid redirect_uri: {}", queries.redirect_uri);
        return Ok(Response::builder(400).build());
    }
    let auth_code: String = uuid::Uuid::new_v4().simple().to_string();
    let tree = state.db.open_tree("default")?;
    if let Some(contact_id) = req.session().get::<u32>("contact_id") {
        tree.insert(&auth_code, &contact_id.to_le_bytes())?;
        tree.insert(contact_id.to_le_bytes(), &*auth_code)?;
        // is it really required to save both pairs?
        log::info!("/authorize Redirected");
        Ok(Redirect::new(format!(
            "{}?state={}&code={auth_code}",
            queries.redirect_uri, queries.state
        ))
        .into())
    } else {
        log::info!("/authorize showing login screen");
        return Ok(Response::builder(200)
            .body(Body::from_file(Path::new("./static/").join("login.html")).await?)
            .build());
    }
}

async fn token_fn(mut req: Request<State>) -> tide::Result {
    let code: Option<String> = {
        let q: TokenQuery = req.query()?;
        if q.code.is_none() {
            let q: TokenQuery = req.take_body().into_form().await?;
            q.code
        } else {
            q.code
        }
    };
    let state = req.state();
    if let Some(code) = code {
        let client_id: String;
        let client_secret: String;
        if let Some(auth) = req.header("authorization") {
            let auth = auth.as_str().to_string();
            log::debug!("/token authentication header raw: {auth}");
            let decoded =
                base64::engine::general_purpose::STANDARD.decode(auth.replacen("Basic ", "", 1))?;
            let decoded = String::from_utf8(decoded)?;
            log::debug!("/token Decoded auth header into utf8: {decoded}");
            let decoded: Vec<&str> = decoded.split(':').collect();
            if decoded.len() < 2 {
                log::info!("/token Not enough tokens in the decoded Auth header");
                return Ok(Response::builder(400).build());
            }
            client_id = decoded[0].to_string();
            client_secret = decoded[1].to_string();
            if client_id != state.config.oauth.client_id {
                log::info!("/token returned 401 because client_ids were inconsistent");
                return Ok(Response::builder(401).build());
            }
            if client_secret != state.config.oauth.client_secret {
                log::info!("/token returned 401 because client_secrets were inconsistent");
                return Ok(Response::builder(401).build());
            }
            let tree = state.db.open_tree("default")?;
            log::debug!("/token Opened default tree in sled");
            if let Some(data) = tree.get(code)? {
                let user = Contact::load_from_db(
                    &state.dc_context,
                    ContactId::new(u32::from_le_bytes(data[..].try_into()?)),
                    // this should be in parrallel with the convert in /authorize
                )
                .await?;
                return Ok(Response::builder(200)
                    .body(Body::from_json(&json!({
                        "access_token": uuid::Uuid::new_v4().to_string(),
                        "token_type": "bearer",
                        "expires_in": 1,
                        "info": {
                            "username": user.get_name(),
                            "email": user.get_addr(),
                        }
                    }))?)
                    .build());
            }
            log::info!("/token Returning 400 because there is no such code in our sled db");
            return Ok(Response::builder(400).build());
        }
        log::info!("/token Returning 401 because there is no auth header");
        return Ok(Response::builder(401).build());
    }
    log::info!("/token returned 400 because there was not 'code' in queries");
    Ok(Response::builder(400).build())
}

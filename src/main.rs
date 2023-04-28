mod config;
mod queries;

use std::env::{args, current_dir};
use std::fs::read;
use std::path::PathBuf;
use std::str::from_utf8;

use anyhow::Context as _;
use base64::Engine;
use deltachat::config::Config;
use deltachat::contact::{Contact, ContactId};
use deltachat::context::{Context, ContextBuilder};
use deltachat::chat::{create_group_chat, ProtectionStatus, get_chat_contacts, ChatId};
use tide::log;
use tide::prelude::*;
use tide::{Body, Redirect, Request, Response};

use crate::config::BotConfig;
use crate::queries::*;

#[derive(Clone)]
struct State {
    db: sled::Db,
    dc_context: Context,
    config: BotConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    femme::start();
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

    log::info!("Starting the bot. Address: {}", botconfig.email);
    log::info!("Open bot db");
    let db = sled::open(&botconfig.oauth_db)?;
    log::info!("Open deltachat context");
    let ctx = ContextBuilder::new(botconfig.deltachat_db.clone().into())
        .open()
        .await
        .context("Creating context failed")?;
    let state = State {
        db,
        dc_context: ctx.clone(),
        config: botconfig.clone(),
    };
    let mut backend = tide::with_state(state);
    if botconfig.enable_request_logging == Some(true) {
        backend.with(tide::log::LogMiddleware::new());
    }
    backend
        .at("/")
        .get(|_| async { Ok("Hello, this is an instance of a 'login with deltachat'-Bot.") });
    backend.at("/authorize").get(authorize_fn);
    backend.at("/token").post(token_fn);
    backend.at("/webhook").post(webhook_fn);
    backend.at("/requestQR").get(requestqr_fn);
    backend.at("/checkStatus").get(check_status_fn);


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

    backend.listen(botconfig.listen_addr.clone()).await?;
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn requestqr_fn(req: Request<State>) -> tide::Result {
    let mut uuid = uuid::Uuid::new_v4().simple().to_string();
    uuid.truncate(5);
    let group_name = format!("LoginBot group {uuid}");
    let state = req.state();
    let group = create_group_chat(&state.dc_context, ProtectionStatus::Protected, &group_name);
    todo!() // I'm unsure what sort of String does the DC QR generator return so I stopped here.
            // --Farooq
}

async fn check_status_fn(req: Request<State>) -> tide::Result {
    let session = req.session();
    if let Some(groupId) = session.get::<String>("groupId") {
        let dc_context = req.state().dc_context;
        let chat_members = get_chat_contacts(&dc_context, ChatId::new(u32::from_str_radix(&groupId, 10)?)).await?;
        match chat_members.len() {
            number_of_members => {
                log::error!("{}", format!("This must not happen. There is/are {number_of_members} in the group {groupId}"));
                return Err(tide::Error::from_str(500, "Some internal error occured..."));
            }
            1 => {
                return Ok(Response::builder(200).body(Body::from_string("Not yet...".to_string())).build());
            }
            2 => {
                let i = {
                    if chat_members[0] == deltachat::contact::ContactId::SELF {
                        1
                    } else {
                        0
                    }
                };
                session.insert("contactId", chat_members[i].to_string().clone());
                Ok(Response::builder(200).body(Body::empty()).build())
            }
        }
    } else {
        return Ok(Response::builder(400).body(Body::empty()).build());
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
        return Ok(Response::builder(400).build());
    }
    if queries.redirect_uri != config.oauth.redirect_uri {
        return Ok(Response::builder(400).build());
    }
    let auth_code: String = uuid::Uuid::new_v4().simple().to_string();
    let tree = state.db.open_tree("default")?;
    if let Some(contact_id) = req.session().get::<String>("contact_id") {
        tree.insert(&auth_code, &*contact_id)?;
        Ok(Redirect::new(format!(
            "{}?state={}&code={auth_code}",
            queries.redirect_uri, queries.state
        ))
        .into())
    } else {
        return Ok(Response::builder(400).build());
    }
}

async fn token_fn(req: Request<State>) -> tide::Result {
    let queries: TokenQuery = req.query()?;
    let state = req.state();
    if let Some(code) = queries.code {
        let client_id: String;
        let client_secret: String;
        if let Some(auth) = req.header("authorization") {
            let auth = auth.as_str().to_string();
            let decoded =
                base64::engine::general_purpose::STANDARD.decode(auth.replacen("Basic", "", 1))?;
            let decoded = String::from_utf8(decoded)?;
            let decoded: Vec<&str> = decoded.split(":").collect();
            if decoded.len() < 2 {
                return Ok(Response::builder(400).build());
            }
            client_id = decoded[0].to_string();
            client_secret = decoded[1].to_string();
            if client_id != state.config.oauth.client_id {
                return Ok(Response::builder(401).build());
            }
            if client_secret != state.config.oauth.client_secret {
                return Ok(Response::builder(401).build());
            }
            let tree = state.db.open_tree("default")?;
            if let Some(data) = tree.get(code)? {
                let user = Contact::load_from_db(
                    &state.dc_context,
                    ContactId::new(u32::from_be_bytes(data[..].try_into()?)),
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
            return Ok(Response::builder(400).build());
        }
        return Ok(Response::builder(401).build());
    }
    Ok(Response::builder(400).build())
}

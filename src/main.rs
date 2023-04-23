mod config;
mod queries;

use anyhow::{Context as _, Result};
use deltachat::chat::{self, Chat, ChatId};
use deltachat::config::Config;
use deltachat::contact::Contact;
use deltachat::context::{Context, ContextBuilder};

use regex::Regex;
use std::env::{args, current_dir};
use std::fs::read;
use std::path::PathBuf;
use std::str::from_utf8;
use tide::prelude::*;
use tide::{Body, Redirect, Request, Response};

use crate::config::BotConfig;
use crate::queries::*;

#[derive(Clone)]
struct State {
    db: sled::Db,
    dc_context: Context,
    config: BotConfig,
    b64engine: base64::Engine,
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

    println!("Starting the bot. Address: {}", botconfig.email);
    let db = sled::open(botconfig.db)?;
    let ctx = ContextBuilder::new(botconfig.deltachat_db.into())
        .open()
        .await
        .context("Creating context failed")?;
    let events_emitter = ctx.get_event_emitter();
    let emitter_ctx = ctx.clone();
    let re = Regex::new(r".*\((<topic_id>\d+)\)$")?;
    let mut state = State {
        db,
        dc_context: ctx,
        config: botconfig,
        b64engine: base64::Engine::new(),
    };
    let mut backend = tide::with_state(state);
    backend.at("/authorize").get(authorize_fn);
    backend.at("/token").post(token_fn);
    backend.at("/webhook").post(webhook_fn);

    if !ctx.get_config_bool(Config::Configured).await? {
        ctx.set_config(Config::Addr, Some(&botconfig.email)).await?;
        ctx.set_config(Config::MailPw, Some(&botconfig.password))
            .await?;
        ctx.set_config(Config::Bot, Some("1")).await?;
        ctx.set_config(Config::E2eeEnabled, Some("1")).await?;
        ctx.configure().await.context("configuration failed...")?;
    }

    ctx.start_io().await;
    tokio::signal::ctrl_c().await?;
    ctx.stop_io().await;
    Ok(())
}

async fn webhook_fn(req: Request<State>) -> tide::Result {
    Ok(Response::builder::build(200).build())
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
    let contact_id: &str = &req.session().get::<String>("contact_id").unwrap();
    tree.insert(&auth_code, contact_id)?;
    Ok(Redirect::new(format!(
        "{}?state={}&code={auth_code}",
        queries.redirect_uri, queries.state
    ))
    .into())
}

async fn token_fn(req: Request<State>) -> tide::Result {
    let queries: TokenQuery = req.query()?;
    let state = req.state();
    if let Some(code) = queries.code {
        let client_id: String = "".to_string();
        let client_secret: String = "".to_string();
        if let Some(auth) = req.header("authorization") {
            let auth = auth.as_str().to_string();
            let decoded = state.b64engine.decode(auth.replacen("Basic", "", 1));
            let decoded = String::from_utf8(decoded)?;
            let decoded = decoded.split(":").collect();
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
                let user = Contact::load_from_db(&state.dc_context, data);
                return Ok(Response::builder(200).body(Body::from_json(&json!({ "access_token": uuid::Uuid::new_v4().to_string(), "token_type": "bearer", "expires_in": 1, "info": {
                    "username": user.get_name(),
                    "email": user.get_addr(),
                }}))));
            }
            return Ok(Response::builder(400).build());
        }
        return Ok(Response::builder(401).build());
    }
    Ok(Response::builder(400).build())
}

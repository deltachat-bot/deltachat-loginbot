mod config;
mod queries;

use anyhow::{Context as _, Result};
use deltachat::contact::Contact;
use deltachat::config::Config;
use deltachat::chat::{self, Chat, ChatId};
use deltachat::context::{Context, ContextBuilder};
use deltachat::message::{Message, MsgId, Viewtype};
use deltachat::EventType;

use std::env::{args, current_dir};
use std::fs::read;
use std::path::PathBuf;
use std::str::from_utf8;
use regex::Regex;
use tide::{Request, Response, Redirect};

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

    println!(
        "Starting the bot. Address: {}",
        botconfig.email
    );
    let db = sled::open(botconfig.db)?;
    let ctx = ContextBuilder::new(botconfig.deltachat_db.into())
        .open()
        .await
        .context("Creating context failed")?;
    let events_emitter = ctx.get_event_emitter();
    let emitter_ctx = ctx.clone();
    let re = Regex::new(r".*\((<topic_id>\d+)\)$")?;
    tokio::spawn(async move {
        while let Some(event) = events_emitter.recv().await {
            match event.typ {
                EventType::IncomingMsg { chat_id, msg_id } => {
                    if let Err(err) = handle_message(&emitter_ctx, chat_id, msg_id, &re).await {
                        println!("error handling message: {err}");
                    }
                }
                _ => {}
            }
        }
    });
   
    let mut state = State {
        db,
        dc_context: ctx,
        config: botconfig,
    };
    let mut backend = tide::with_state(state);
    backend.at("/authorize").get(authorize_fn);
    backend.at("/token").post(token_fn);
    backend.at("/webhook").post(webhook_fn);

    if !ctx.get_config_bool(Config::Configured).await? {
        ctx.set_config(Config::Addr, Some(&botconfig.email)).await?;
        ctx.set_config(Config::MailPw, Some(&botconfig.password)).await?;
        ctx.set_config(Config::Bot, Some("1")).await?;
        ctx.set_config(Config::E2eeEnabled, Some("1")).await?;
        ctx.configure().await.context("configuration failed...")?;
    }

    ctx.start_io().await;
    tokio::signal::ctrl_c().await?;
    ctx.stop_io().await;
    Ok(())
}

async fn handle_message(ctx: &Context, chat_id: ChatId, msg_id: MsgId, re: &Regex) -> anyhow::Result<()> {
    let chat = Chat::load_from_db(ctx, chat_id).await?;
    let captures = re.captures(chat.get_name());
    let topic_id;
    if let Some(captures) = re.captures(chat.get_name()) {
        if &captures["topic_id"] != "" {
            topic_id = &captures["topic_id"];
        } else {
            println!("Chat name doesn't match: {}", chat.get_name());
            return Ok(());
        }
    } 
    let incoming_msg = Message::load_from_db(ctx, msg_id)
        .await?;
    let contact = Contact::load_from_db(ctx, incoming_msg.get_from_id()).await?;


    let mut msg = Message::new(Viewtype::Text);
    msg.set_text(incoming_msg);
    println!("Sending back a message...");
    chat::send_msg(ctx, chat_id, &mut msg).await?;
    Ok(())
}

async fn webhook_fn(req: Request<State>) -> tide::Result {
    todo!()
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
    Ok(Redirect::new(format!("{}?state={}&code={auth_code}", queries.redirect_uri, queries.state)).into())
}

async fn token_fn(req: Request<State>) -> tide::Result {
    todo!()
}

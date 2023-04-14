use anyhow::{Context as _, Result};
use deltachat::contact::Contact;
use deltachat::config::Config;
use deltachat::chat::{self, Chat, ChatId};
use deltachat::context::{Context, ContextBuilder};
use deltachat::message::{Message, MsgId, Viewtype};
use deltachat::EventType;

use serde::Deserialize;
use std::env::{args, current_dir};
use std::fs::read;
use std::path::PathBuf;
use std::str::from_utf8;
use regex::Regex;

#[derive(Deserialize)]
struct BotConfig {
    email: String,
    password: String,
    deltachat_db: String,
    oauth_db: String,
    notifier: NotifierConfig,
    oauth: OAuthConfig,
}

#[derive(Deserialize)]
struct NotifierConfig {
    discouse_base_url: String,
    api_key: String,
    api_username: String,
    enabled_contact_email_addresses: bool
}

#[derive(Deserialize)]
struct OAuthConfig {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
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
    let discourse_user_data = fetch(format!("/admin/users/list/active.json?filter={}", contact.addr));
    let topic_data = fetch(format!("/t/{topic_id}.json"));


    let mut msg = Message::new(Viewtype::Text);
    msg.set_text(incoming_msg);
    println!("Sending back a message...");
    chat::send_msg(ctx, chat_id, &mut msg).await?;
    Ok(())
}

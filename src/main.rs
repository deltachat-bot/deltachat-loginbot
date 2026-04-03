mod shutdown_signal;

use std::env::{args, current_dir};
use std::fs::read;
use std::path::PathBuf;
use std::str::from_utf8;

use anyhow::Context as _;
use deltachat::config::Config;
use deltachat::context::ContextBuilder;
use deltachat::EventType;
use deltachat_loginbot::{build_router, AppState, BotConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let botconfig: BotConfig;
    {
        let mut config_file_path = current_dir()
            .context("Cannot get current directory")?
            .join("config.toml");
        if let Some(arg) = args().nth(1) {
            config_file_path = PathBuf::from(arg);
        }
        botconfig = toml::from_str(from_utf8(&read(config_file_path)?)?)?;
    }
    let level = botconfig
        .log_level
        .as_deref()
        .and_then(|s| s.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::WARN);
    tracing_subscriber::fmt().with_max_level(level).init();
    let db = sled::open(&botconfig.oauth_db)?;
    let ctx = ContextBuilder::new(botconfig.deltachat_db.clone())
        .open()
        .await
        .context("Creating context failed")?;
    let dc_events = ctx.get_event_emitter();
    let dc_event_task = tokio::spawn(async move {
        while let Some(event) = dc_events.recv().await {
            match event.typ {
                EventType::Error(message) => log::error!("{}", message),
                EventType::Warning(message) => log::warn!("{}", message),
                EventType::Info(message) => log::info!("{}", message),
                event => log::debug!("{:?}", event),
            }
        }
    });
    let static_dir = botconfig
        .static_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("./static"));
    let state: AppState = AppState {
        db,
        dc_context: ctx.clone(),
        config: botconfig.clone(),
        login_html: String::from_utf8(read(static_dir.join("login.html"))?)?,
    };
    let backend = build_router(state, static_dir);

    if !ctx.get_config_bool(Config::Configured).await? {
        log::info!("Configure deltachat context");
        ctx.set_config(Config::Addr, Some(botconfig.email.clone().as_str()))
            .await?;
        ctx.set_config(Config::MailPw, Some(botconfig.password.clone().as_str()))
            .await?;
        ctx.set_config(Config::Bot, Some("1")).await?;
        ctx.configure().await.context("configuration failed...")?;
    }
    // connect to email server
    ctx.start_io().await;
    let listener = tokio::net::TcpListener::bind(botconfig.listen_addr).await?;
    axum::serve(listener, backend)
        .with_graceful_shutdown(shutdown_signal::shutdown_signal())
        .await?;
    log::info!("Shutting Down");
    ctx.stop_io().await;
    dc_event_task.abort();
    Ok(())
}

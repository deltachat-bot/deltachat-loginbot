//! Black-box integration test for the full OAuth2 login flow.
//!
//! Uses two real DC accounts on ci-chatmail.testrun.org:
//!   - a "bot" account that runs the loginbot server
//!   - a "user" account that scans the QR code to log in
//!
//! Run with: cargo test --test login_flow -- --ignored --nocapture

#![recursion_limit = "256"]

use std::time::Duration;

use anyhow::{Context as _, Result};
use deltachat::config::Config;
use deltachat::context::ContextBuilder;
use deltachat::securejoin::join_securejoin;
use deltachat_loginbot::{build_router, AppState, BotConfig, OAuthConfig};
use reqwest::redirect::Policy;

const CHATMAIL_DOMAIN: &str = "ci-chatmail.testrun.org";
const CLIENT_ID: &str = "test-client";
const CLIENT_SECRET: &str = "test-secret";
const REDIRECT_URI: &str = "https://example.com/callback";

async fn configure_account(
    dir: &std::path::Path,
    prefix: &str,
) -> Result<deltachat::context::Context> {
    let db_path = dir.join(format!("{prefix}.db"));
    let ctx = ContextBuilder::new(db_path).open().await?;

    // Provision credentials via the chatmail server's API
    let qr = format!("dcaccount:{CHATMAIL_DOMAIN}");
    deltachat::qr::set_config_from_qr(&ctx, &qr).await?;

    ctx.set_config(Config::Bot, Some("1")).await?;
    ctx.configure().await.context("configure failed")?;
    ctx.start_io().await;
    Ok(ctx)
}

#[test]
fn test_full_login_flow() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(full_login_flow()).unwrap();
}

async fn full_login_flow() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let dir = tempfile::tempdir()?;

    // 1) Configure bot and user accounts
    log::info!("Configuring bot account…");
    let bot_ctx = configure_account(dir.path(), "bot").await?;

    log::info!("Configuring user account…");
    let user_ctx = configure_account(dir.path(), "user").await?;
    user_ctx.set_config(Config::Bot, Some("0")).await?;

    // 2) Start the loginbot server on a random port
    let db = sled::open(dir.path().join("oauth.db"))?;
    let static_dir = dir.path().join("static");
    std::fs::create_dir_all(&static_dir)?;
    std::fs::write(static_dir.join("login.html"), b"<html>login</html>")?;

    let state = AppState {
        db,
        dc_context: bot_ctx.clone(),
        config: BotConfig {
            email: bot_ctx.get_config(Config::Addr).await?.unwrap_or_default(),
            password: "unused-in-test".into(),
            deltachat_db: dir.path().join("bot.db").to_string_lossy().into(),
            oauth_db: dir.path().join("oauth.db").to_string_lossy().into(),
            listen_addr: "127.0.0.1:0".parse()?,
            oauth: OAuthConfig {
                client_id: CLIENT_ID.into(),
                client_secret: CLIENT_SECRET.into(),
                redirect_uri: REDIRECT_URI.into(),
            },
            static_dir: Some(static_dir.clone()),
            log_level: None,
        },
        login_html: "<html>login</html>".into(),
    };
    let router = build_router(state, static_dir);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let base_url = format!("http://127.0.0.1:{port}");

    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    // Give server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 3) Use reqwest with a cookie jar so sessions persist
    let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
    let client = reqwest::Client::builder()
        .cookie_provider(jar)
        .redirect(Policy::none())
        .build()?;

    // 4) GET /requestQr — creates a group, returns invite link
    log::info!("Requesting QR…");
    let resp = client.get(format!("{base_url}/requestQr")).send().await?;
    assert_eq!(resp.status(), 200, "requestQr failed");
    let json: serde_json::Value = resp.json().await?;
    let invite_link = json["link"]
        .as_str()
        .context("no link in requestQr response")?
        .to_string();
    log::info!("Got invite link: {invite_link}");

    // Assert that a second call generates a new link
    let resp2 = client.get(format!("{base_url}/requestQr")).send().await?;
    let json2: serde_json::Value = resp2.json().await?;
    let invite_link_2 = json2["link"].as_str().unwrap().to_string();
    assert_ne!(
        invite_link, invite_link_2,
        "second invocation returned identical QR code"
    );
    // Use the latest invite link for the remainder of the test
    let invite_link = invite_link_2;
    assert!(
        invite_link.starts_with("https://"),
        "expected https invite link, got: {invite_link}"
    );

    // 5) GET /checkStatus — should be waiting (no one joined yet)
    let resp = client.get(format!("{base_url}/checkStatus")).send().await?;
    let json: serde_json::Value = resp.json().await?;
    assert_eq!(json["waiting"], true, "expected waiting, got: {json}");

    // 6) User joins the group via securejoin
    log::info!("User joining via securejoin…");
    join_securejoin(&user_ctx, &invite_link).await?;

    // 7) Poll /checkStatus until the bot sees the new member.
    //    The endpoint returns {"success": true} once a second
    //    member has joined the group.
    log::info!("Polling checkStatus…");
    let mut joined = false;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let resp = client.get(format!("{base_url}/checkStatus")).send().await?;
        let json: serde_json::Value = resp.json().await?;
        if json.get("success").is_some() {
            log::info!("User joined, checkStatus returned: {json}");
            joined = true;
            break;
        }
    }
    assert!(joined, "user was not detected within 60s");

    // 8) GET /authorize — should redirect with ?code=...
    let resp = client
        .get(format!("{base_url}/authorize"))
        .query(&[
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("state", "test123"),
            ("response_type", "code"),
        ])
        .send()
        .await?;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {}",
        resp.status()
    );
    let location = resp
        .headers()
        .get("location")
        .context("no location header")?
        .to_str()?;
    assert!(
        location.starts_with(REDIRECT_URI),
        "bad redirect: {location}"
    );
    assert!(
        location.contains("code="),
        "no code in redirect: {location}"
    );
    assert!(
        location.contains("state=test123"),
        "state lost in redirect: {location}"
    );

    // Extract the auth code from the redirect URL
    let code = url::Url::parse(location)?
        .query_pairs()
        .find_map(|(k, v)| (k == "code").then(|| v.to_string()))
        .context("no code query param")?;
    log::info!("Got auth code: {code}");

    // 9) POST /token — exchange code for user info
    let resp = client
        .post(format!("{base_url}/token"))
        .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
        .form(&[("grant_type", "authorization_code"), ("code", &code)])
        .send()
        .await?;
    assert_eq!(resp.status(), 200, "token exchange failed");
    let json: serde_json::Value = resp.json().await?;
    let email = json["info"]["email"]
        .as_str()
        .context("no email in token response")?;
    let user_addr = user_ctx.get_config(Config::Addr).await?.unwrap_or_default();
    assert_eq!(email, user_addr, "email mismatch");
    log::info!("Token exchange returned email={email}");

    // 10) Second login from the same browser session (same cookie jar).
    //     This is the repeated-login regression: a stale `sent=true` session
    //     key previously prevented `contact_id` from being written, so
    //     /authorize showed the login page instead of redirecting.
    log::info!("--- Second login attempt (same browser session) ---");
    let resp = client.get(format!("{base_url}/requestQr")).send().await?;
    assert_eq!(resp.status(), 200, "second requestQr failed");
    let json: serde_json::Value = resp.json().await?;
    let invite_link2 = json["link"]
        .as_str()
        .context("no link in second requestQr response")?
        .to_string();
    log::info!("Got second invite link: {invite_link2}");

    join_securejoin(&user_ctx, &invite_link2).await?;

    let mut joined2 = false;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let resp = client.get(format!("{base_url}/checkStatus")).send().await?;
        let json: serde_json::Value = resp.json().await?;
        if json.get("success").is_some() {
            log::info!("Second join confirmed: {json}");
            joined2 = true;
            break;
        }
    }
    assert!(joined2, "second login: user was not detected within 60s");

    let resp = client
        .get(format!("{base_url}/authorize"))
        .query(&[
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("state", "test456"),
            ("response_type", "code"),
        ])
        .send()
        .await?;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT,
        "second login: expected redirect, got {}",
        resp.status()
    );
    let location2 = resp
        .headers()
        .get("location")
        .context("second login: no location header")?
        .to_str()?;
    assert!(
        location2.contains("code="),
        "second login: no code in redirect: {location2}"
    );
    assert!(
        location2.contains("state=test456"),
        "second login: state lost: {location2}"
    );
    log::info!("Second login redirected to: {location2}");

    // Cleanup
    bot_ctx.stop_io().await;
    user_ctx.stop_io().await;

    Ok(())
}

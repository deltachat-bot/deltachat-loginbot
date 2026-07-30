//! Black-box integration test for the full OAuth2 login flow.
//!
//! Uses real DC accounts on ci-chatmail.testrun.org:
//!   - a "bot" account that runs the loginbot server
//!   - a "user" account that scans the QR code to log in
//!   - a "newcomer" account showing up after the legacy map is sealed
//!
//! Run with: cargo test --test login_flow -- --ignored --nocapture
//! A debug build needs RUST_MIN_STACK=16777216.

#![recursion_limit = "256"]

use std::time::Duration;

use anyhow::{Context as _, Result};
use deltachat::config::Config;
use deltachat::context::ContextBuilder;
use deltachat::securejoin::{get_securejoin_qr, join_securejoin};
use deltachat_loginbot::{build_router, seal_legacy_map, AppState, BotConfig, OAuthConfig};
use reqwest::redirect::Policy;

const CHATMAIL_DOMAIN: &str = "ci-chatmail.testrun.org";
const CLIENT_ID: &str = "test-client";
const CLIENT_SECRET: &str = "test-secret";
const REDIRECT_URI: &str = "https://example.com/callback";
const SHADOW_DOMAIN: &str = "loginbot.testrun.org";

async fn configure_account(
    dir: &std::path::Path,
    prefix: &str,
) -> Result<deltachat::context::Context> {
    let db_path = dir.join(format!("{prefix}.db"));
    let ctx = ContextBuilder::new(db_path).open().await?;

    let qr = format!("dcaccount:{CHATMAIL_DOMAIN}");
    ctx.add_transport_from_qr(&qr).await?;
    ctx.start_io().await;
    Ok(ctx)
}

/// A browser session of its own, with a cookie jar.
fn new_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .cookie_provider(std::sync::Arc::new(reqwest::cookie::Jar::default()))
        .redirect(Policy::none())
        .build()?)
}

/// Returns the lowercase fingerprint of the profile's own key, read
/// from its invite QR, whose fragment starts with it.
async fn self_fingerprint(ctx: &deltachat::context::Context) -> Result<String> {
    let qr = get_securejoin_qr(ctx, None).await?;
    let fragment = qr.split('#').nth(1).context("No fragment in invite QR")?;
    Ok(fragment
        .split('&')
        .next()
        .context("Empty QR fragment")?
        .to_lowercase())
}

/// Drives one login to the end and returns the `info` of `/token`.
async fn login(
    base_url: &str,
    client: &reqwest::Client,
    user_ctx: &deltachat::context::Context,
    state: &str,
) -> Result<serde_json::Value> {
    let resp = client.get(format!("{base_url}/requestQr")).send().await?;
    assert_eq!(resp.status(), 200, "requestQr failed");
    let json: serde_json::Value = resp.json().await?;
    let invite_link = json["link"].as_str().context("No invite link")?.to_string();
    join_securejoin(user_ctx, &invite_link).await?;

    let mut joined = false;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let resp = client.get(format!("{base_url}/checkStatus")).send().await?;
        let json: serde_json::Value = resp.json().await?;
        if let Some(error) = json.get("error") {
            anyhow::bail!("Login refused by /checkStatus: {error}");
        }
        if json.get("success").is_some() {
            joined = true;
            break;
        }
    }
    assert!(joined, "user was not detected within 60s");

    let resp = client
        .get(format!("{base_url}/authorize"))
        .query(&[
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("state", state),
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
        .context("No location header")?
        .to_str()?;
    assert!(
        location.starts_with(REDIRECT_URI),
        "bad redirect: {location}"
    );
    assert!(location.contains(&format!("state={state}")), "state lost");
    let code = url::Url::parse(location)?
        .query_pairs()
        .find_map(|(k, v)| (k == "code").then(|| v.to_string()))
        .context("No code query param")?;

    let resp = client
        .post(format!("{base_url}/token"))
        .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
        .form(&[("grant_type", "authorization_code"), ("code", &code)])
        .send()
        .await?;
    assert_eq!(resp.status(), 200, "token exchange failed");
    let json: serde_json::Value = resp.json().await?;
    Ok(json["info"].clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_full_login_flow() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let dir = tempfile::tempdir()?;

    // Configure bot and user accounts
    log::info!("Configuring bot account…");
    let bot_ctx = configure_account(dir.path(), "bot").await?;
    bot_ctx.set_config(Config::Bot, Some("1")).await?;

    log::info!("Configuring user account…");
    let user_ctx = configure_account(dir.path(), "user").await?;

    // Start the loginbot server on a random port
    let db = sled::open(dir.path().join("oauth.db"))?;
    let static_dir = dir.path().join("static");
    std::fs::create_dir_all(&static_dir)?;
    std::fs::write(static_dir.join("login.html"), b"<html>login</html>")?;

    let state = AppState {
        db: db.clone(),
        dc_context: bot_ctx.clone(),
        config: BotConfig {
            email: bot_ctx.get_config(Config::Addr).await?.unwrap_or_default(),
            password: "unused-in-test".into(),
            deltachat_db: dir.path().join("bot.db"),
            oauth_db: dir.path().join("oauth.db"),
            listen_addr: "127.0.0.1:0".parse()?,
            shadow_domain: SHADOW_DOMAIN.into(),
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
    let router = build_router(state, static_dir.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let base_url = format!("http://127.0.0.1:{port}");

    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    // Use reqwest with a cookie jar so sessions persist
    let client = new_client()?;

    // GET /requestQr — creates a group, returns a fresh invite link
    log::info!("Requesting QR…");
    let resp = client.get(format!("{base_url}/requestQr")).send().await?;
    assert_eq!(resp.status(), 200, "requestQr failed");
    let json: serde_json::Value = resp.json().await?;
    let invite_link = json["link"].as_str().context("No invite link")?;
    assert!(
        invite_link.starts_with("https://"),
        "expected https invite link, got: {invite_link}"
    );
    let invite_link = invite_link.to_string();
    let resp = client.get(format!("{base_url}/requestQr")).send().await?;
    let json: serde_json::Value = resp.json().await?;
    assert_ne!(
        Some(invite_link.as_str()),
        json["link"].as_str(),
        "second invocation returned identical QR code"
    );

    // GET /checkStatus — should be waiting (no one joined yet)
    let resp = client.get(format!("{base_url}/checkStatus")).send().await?;
    let json: serde_json::Value = resp.json().await?;
    assert_eq!(json["waiting"], true, "expected waiting, got: {json}");

    // The user scans the QR and the login runs to the end. The
    // identity is the key, so the address is derived from it.
    let info = login(&base_url, &client, &user_ctx, "test123").await?;
    let email = info["email"].as_str().context("No email")?.to_string();
    let user_addr = user_ctx.get_config(Config::Addr).await?.unwrap_or_default();
    let fingerprint = self_fingerprint(&user_ctx).await?;
    assert_eq!(info["id"], fingerprint, "id is not the fingerprint");
    assert_eq!(email, format!("{fingerprint}@{SHADOW_DOMAIN}"));
    assert_ne!(email, user_addr, "real address leaked");
    log::info!("Token exchange returned email={email}");

    // Second login from the same browser session (same cookie jar).
    // This is the repeated-login regression: a stale `sent=true` session
    // key previously prevented `contact_id` from being written, so
    // /authorize showed the login page instead of redirecting.
    log::info!("--- Second login attempt (same browser session) ---");
    let info = login(&base_url, &client, &user_ctx, "test456").await?;
    assert_eq!(info["email"], email, "identity changed between logins");

    // Sealing, as the first start on an existing database does: the
    // user is known to the bot by now, so they keep the address the
    // relying party has their account under.
    assert_eq!(seal_legacy_map(&bot_ctx, &db).await?, 1, "one address kept");
    let client2 = new_client()?;
    let info = login(&base_url, &client2, &user_ctx, "test789").await?;
    assert_eq!(info["email"], user_addr, "sealed address not returned");
    assert_eq!(info["id"], fingerprint, "id is not the fingerprint");

    // A key that was not in the map when it was sealed cannot get in
    // anymore, so somebody showing up later is named by their key.
    let newcomer_ctx = configure_account(dir.path(), "newcomer").await?;
    let newcomer_addr = newcomer_ctx
        .get_config(Config::Addr)
        .await?
        .unwrap_or_default();
    let newcomer_fingerprint = self_fingerprint(&newcomer_ctx).await?;
    let info = login(&base_url, &client2, &newcomer_ctx, "test999").await?;
    assert_eq!(
        info["email"],
        format!("{newcomer_fingerprint}@{SHADOW_DOMAIN}"),
        "newcomer was not named by their key"
    );
    assert_ne!(info["email"], newcomer_addr, "real address leaked");
    assert_eq!(
        seal_legacy_map(&bot_ctx, &db).await?,
        1,
        "sealing again adopted an address"
    );

    // Cleanup
    bot_ctx.stop_io().await;
    user_ctx.stop_io().await;
    newcomer_ctx.stop_io().await;

    Ok(())
}

/// A fresh setup has nothing to record, so the map is sealed empty and
/// everybody is named by their key from the start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_seal_on_fresh_database() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let ctx = ContextBuilder::new(dir.path().join("fresh.db"))
        .open()
        .await?;
    let db = sled::open(dir.path().join("oauth.db"))?;
    assert_eq!(seal_legacy_map(&ctx, &db).await?, 0);
    assert_eq!(seal_legacy_map(&ctx, &db).await?, 0, "not idempotent");
    Ok(())
}

/// The `identities` tree of an older deployment is transferred into the
/// account, whatever case its fingerprints come in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_seal_transfers_identities() -> Result<()> {
    const FP: &str = "9D1C2E3F4A5B6C7D8E9F0A1B2C3D4E5F60718293";
    let dir = tempfile::tempdir()?;
    let ctx = ContextBuilder::new(dir.path().join("dc.db")).open().await?;
    let db = sled::open(dir.path().join("oauth.db"))?;
    db.open_tree("identities")?
        .insert(FP, b"user@example.org".as_slice())?;

    assert_eq!(seal_legacy_map(&ctx, &db).await?, 1);
    assert_eq!(
        ctx.get_ui_config(&format!("ui.loginbot.addr.{}", FP.to_lowercase()))
            .await?,
        Some("user@example.org".to_string())
    );
    Ok(())
}

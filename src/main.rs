mod app;
mod auth;
mod config;
mod demo;
mod event;
mod media_keys;
mod spotify;
mod sync;
mod ui;
mod updater;
mod visualizer;

use std::sync::{Arc, atomic::AtomicBool};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::sync::{Notify, mpsc};
use tracing::{error, info};

use crate::{
    app::App,
    config::Config,
    spotify::{api::Api, internal::Internal, observer, player::LocalPlayer, sink::SampleTap},
};

#[derive(Parser)]
#[command(
    name = "talyxel",
    version,
    about = "Talyxel Sound — Spotify in your terminal"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Skip the update check for this run.
    #[arg(long)]
    no_update_check: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Link (or re-link) your Spotify account in the browser.
    Login,
    /// Remove the stored Spotify credentials from the OS keyring.
    Logout,
    /// Check GitHub Releases for a newer version and install it.
    Update,
    /// Print where config and logs are stored.
    Paths,
    /// Preview the interface with mock data (no Spotify account needed).
    Demo,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Both rustls crypto backends end up enabled in the dependency tree, so rustls can't pick
    // one on its own; librespot's HTTPS and websocket clients would panic without this.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();
    let _log_guard = init_logging();
    let mut cfg = Config::load_or_create()?;
    if cli.no_update_check {
        cfg.check_updates = false;
    }

    match cli.command {
        Some(Command::Login) => {
            auth::login(&cfg).await?;
            println!("Spotify account linked. Run `talyxel` to start.");
            Ok(())
        }
        Some(Command::Logout) => {
            if auth::logout(&cfg)? {
                println!("Stored Spotify credentials removed.");
            } else {
                println!("No stored credentials found.");
            }
            Ok(())
        }
        Some(Command::Update) => {
            println!("Current version: v{}", updater::current_version());
            let result =
                tokio::task::spawn_blocking(move || updater::install(&cfg, true)).await??;
            match result {
                Some(v) => println!("Updated to v{v}."),
                None => println!("Already up to date."),
            }
            Ok(())
        }
        Some(Command::Paths) => {
            println!(
                "config: {}",
                config::config_dir()?.join("config.toml").display()
            );
            println!(
                "logs:   {}",
                config::data_dir()?.join("talyxel.log").display()
            );
            Ok(())
        }
        Some(Command::Demo) => {
            let (tx, rx) = mpsc::unbounded_channel();
            let mut app = demo::app(tx);
            // Preview the configured colors; `t` still switches themes, without saving.
            app.theme = ui::theme::Theme::from_config(&cfg.theme, &cfg.colors).0;
            app.cfg.theme = cfg.theme.clone();
            app.cfg.colors = cfg.colors.clone();
            let mut terminal = ratatui::init();
            let result = app::run(&mut terminal, &mut app, rx, None).await;
            ratatui::restore();
            result
        }
        None => run_tui(cfg).await,
    }
}

async fn run_tui(cfg: Config) -> Result<()> {
    let token = auth::obtain_token().await?;

    let mut api = Api::new(
        auth::to_rspotify_token(&token),
        auth::DESKTOP_CLIENT_ID,
        |refresh| auth::save_refresh_token(&refresh),
    );
    match auth::obtain_own_app_token(&cfg).await {
        Ok(Some((own, client_id))) => {
            let id = client_id.clone();
            api = api.with_own_app(auth::to_rspotify_token(&own), &client_id, move |refresh| {
                auth::save_own_app_refresh_token(&id, &refresh)
            });
        }
        Ok(None) => {}
        Err(e) => {
            error!("could not use the configured client_id: {e:#}");
            println!("Could not use your own Spotify app ({e:#}); using the shared client.");
        }
    }

    println!(
        "Connecting to Spotify as Connect device \"{}\"…",
        cfg.device_name
    );
    let tap = SampleTap::default();
    let (local, player_events, local_error) =
        match LocalPlayer::start(&cfg, &token.access_token, tap.clone()).await {
            Ok((p, ev)) => (Some(p), Some(ev), None),
            Err(e) => {
                error!("local playback unavailable: {e:#}");
                (
                    None,
                    None,
                    Some(format!("Local playback unavailable: {e:#}")),
                )
            }
        };
    // The Web API refuses the desktop client, so the library, search and Connect go through
    // the playback session whenever there is one.
    let mut cluster_updates = None;
    if let Some(p) = &local {
        let mut internal = Internal::new(p.session.clone());
        match observer::start(&token.access_token).await {
            Ok((session, updates)) => {
                internal = internal.with_observer(session);
                cluster_updates = Some(updates);
            }
            Err(e) => error!("Spotify Connect observer unavailable: {e:#}"),
        }
        api = api.with_internal(internal);
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let local_active = Arc::new(AtomicBool::new(false));
    let poll_wake = Arc::new(Notify::new());

    sync::spawn_remote_poller(
        api.clone(),
        &cfg,
        local_active.clone(),
        poll_wake.clone(),
        tx.clone(),
        cluster_updates,
    );
    sync::spawn_library_watcher(api.clone(), &cfg, tx.clone());
    updater::spawn_check(cfg.clone(), tx.clone());
    let media = media_keys::spawn(tx.clone());

    let (_, theme_warnings) = ui::theme::Theme::from_config(&cfg.theme, &cfg.colors);
    let mut app = App::new(cfg, api, local, tap, local_active, poll_wake, tx, media);
    app.config_path = config::config_path().ok();
    if let Some(w) = theme_warnings.first() {
        app.set_error(w.clone());
    }
    if let Some(e) = local_error {
        app.set_error(e);
    }

    let mut terminal = ratatui::init();
    let result = app::run(&mut terminal, &mut app, rx, player_events).await;
    ratatui::restore();
    app.shutdown();
    info!("exiting");
    result
}

fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = config::data_dir().ok()?;
    let appender = tracing_appender::rolling::never(dir, "talyxel.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = tracing_subscriber::EnvFilter::try_from_env("TALYXEL_LOG")
        // rspotify logs every request at info level, access token included.
        .unwrap_or_else(|_| "info,librespot=info,rspotify_http=warn".into());
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    Some(guard)
}

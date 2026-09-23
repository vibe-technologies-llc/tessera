use anyhow::Context;
use gpui::Application;
use tessera_timeline::Project;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    tessera_media::init().context("initialising media backend")?;
    tracing::info!(
        hw_accels = ?tessera_media::available_hw_accels(),
        "media backend ready"
    );

    Application::new().run(|cx| {
        tessera_ui::init(cx);
        if let Err(error) = tessera_ui::open_main_window(Project::new("Untitled"), cx) {
            tracing::error!(%error, "failed to open main window");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
    Ok(())
}

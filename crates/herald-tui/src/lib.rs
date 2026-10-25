pub mod app;
pub mod browser;
pub mod ui;

use std::path::PathBuf;

pub fn run(config: Option<PathBuf>, account: Option<String>) -> anyhow::Result<()> {
    let herald = herald_core::Herald::open(config.as_deref())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let mut app = app::App::new(herald, runtime, account);
    ratatui::run(move |terminal| app.run(terminal))
}

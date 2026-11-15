use clap::Parser;

use novnc::{Args, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    let args = Args::parse();
    let config = Config::from_args(args)?;

    novnc::run(config).await
}

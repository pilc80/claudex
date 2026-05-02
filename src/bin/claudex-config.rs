use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    claudex::run_config_binary().await
}

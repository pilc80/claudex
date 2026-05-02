use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    claudex::run_from_argv0().await
}

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    claudex::image_mcp::run().await
}

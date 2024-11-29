use anyhow::{Ok, Result};
use dotenv;
use ethers::types::Address;
use revm_playground::{logger::setup_logger, watching::watching_pool};

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    setup_logger()?;
    // parse() 是 Rust 中的一个通用方法，用于将字符串解析成其他类型。
    let weth: Address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".parse()?;

    watching_pool(weth).await?;
    Ok(())
}

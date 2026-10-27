// Cargo.toml: xrat-sdk = "0.28", serde_json = "1"
use xrat_sdk::{config::parse_link, engines::xray};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let node = parse_link(
        "vless://11111111-1111-1111-1111-111111111111@example.com:443#edge",
    )?.ok_or("expected a node")?;

    // Генерируем конфиг Xray с локальным SOCKS-портом 1080 и HTTP 8080
    let config = xray::generate_runtime_config(&node, 1080, Some(8080))?;
    println!("{}", serde_json::to_string_pretty(&config)?);
    Ok(())
}
mod routes;

use anyhow::Result;
use axum::serve;
use mamba_bus::Bus;
use mamba_lake::Lake;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("open_mamba=info".parse()?))
        .json()
        .init();

    let db_path = std::env::var("MAMBA_DB_PATH")
        .unwrap_or_else(|_| "data/mamba.duckdb".to_string());

    std::fs::create_dir_all("data")?;
    let lake = Lake::open(&db_path)?;
    let bus = Bus::new(lake.clone());

    let app = routes::build(lake, bus)
        .layer(CorsLayer::permissive());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(1337);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!("open-mamba listening on http://0.0.0.0:{port}");

    serve(listener, app).await?;
    Ok(())
}

use std::{net::SocketAddr, path::PathBuf};
use vacancy_search_backend::{AppState, open_database, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("VACANCY_PORT")
        .unwrap_or_else(|_| "48173".into())
        .parse::<u16>()?;
    let data = std::env::var_os("VACANCY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                .unwrap_or_else(std::env::temp_dir)
                .join("VacancySearch/vacancy.sqlite3")
        });
    let assets = std::env::var_os("VACANCY_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../frontend/dist"));
    if !assets.join("index.html").exists() {
        eprintln!("Frontend assets absent; API available. Set VACANCY_ASSETS or build frontend.");
    }
    let pool = open_database(&data).await?;
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    let state = AppState {
        pool: pool.clone(),
        hh: vacancy_search_backend::hh::HhClient::from_env(),
        hosts: vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")],
        origins: vec![
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
            "http://127.0.0.1:45173".into(),
        ],
    };
    println!(
        "Vacancy Search: http://{address}; database: {}",
        data.display()
    );
    axum::serve(listener, router(state, Some(&assets)))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    pool.close().await;
    Ok(())
}

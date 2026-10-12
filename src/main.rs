use std::{net::SocketAddr, path::PathBuf};
use vacancy_search_backend::{
    AppState, maintenance, open_database, open_existing_database, router,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|s| s == "maintenance") {
        return maintenance::cli(&arguments[1..]).await;
    }
    let mut require_existing = true;
    let mut lock_path = None;
    let mut physical = None;
    let mut instance = None;
    let mut shutdown_file = None;
    let mut shutdown_token = None;
    let mut flags = arguments.iter();
    while let Some(flag) = flags.next() {
        match flag.to_str() {
            Some("--require-existing-database") => require_existing = true,
            Some("--create-database") => require_existing = false,
            Some("--runtime-lock") => {
                lock_path = Some(PathBuf::from(
                    flags.next().ok_or("Runtime lock path required")?,
                ))
            }
            Some("--expected-database") => {
                physical = Some(PathBuf::from(
                    flags.next().ok_or("Physical DB path required")?,
                ))
            }
            Some("--expected-instance") => {
                instance = Some(
                    flags
                        .next()
                        .ok_or("DB identity required")?
                        .to_string_lossy()
                        .into_owned(),
                )
            }
            Some("--shutdown-file") => {
                shutdown_file = Some(PathBuf::from(flags.next().ok_or("Shutdown path required")?))
            }
            Some("--shutdown-token") => {
                shutdown_token = Some(
                    flags
                        .next()
                        .ok_or("Shutdown nonce required")?
                        .to_string_lossy()
                        .into_owned(),
                )
            }
            _ => return Err("Unknown server argument".into()),
        }
    }
    if physical.is_some() || instance.is_some() || lock_path.is_some() {
        require_existing = true;
    }
    if shutdown_file.is_some() != shutdown_token.is_some() {
        return Err("Shutdown path and nonce must be supplied together".into());
    }
    let runtime_guard = lock_path
        .as_deref()
        .map(maintenance::runtime_lock)
        .transpose()?;
    let port = std::env::var("VACANCY_PORT")
        .unwrap_or_else(|_| "48173".into())
        .parse::<u16>()?;
    let data = std::env::var_os("VACANCY_DB").map(PathBuf::from)
        .ok_or("Explicit VACANCY_DB required; use the installed launch.ps1. No fallback empty database created.")?;
    let assets = std::env::var_os("VACANCY_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../frontend/dist"));
    if !assets.join("index.html").exists() {
        eprintln!("Frontend assets absent; API available. Set VACANCY_ASSETS or build frontend.");
    }
    // Bind before migrating so a port conflict cannot mutate the database.
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    if let (Some(physical), Some(instance)) = (physical.as_deref(), instance.as_deref()) {
        maintenance::validate_managed(&data, physical, instance).await?;
    } else if runtime_guard.is_some() {
        return Err("Managed server requires expected physical DB and identity".into());
    }
    let pool = if require_existing {
        open_existing_database(&data).await?
    } else {
        open_database(&data).await?
    };
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
        .with_graceful_shutdown(async move {
            let console = async {
                if tokio::signal::ctrl_c().await.is_err() {
                    std::future::pending::<()>().await;
                }
            };
            let file_signal = async {
                if let (Some(path), Some(nonce)) = (shutdown_file, shutdown_token) {
                    loop {
                        if std::fs::read_to_string(&path).is_ok_and(|value| value.trim() == nonce) {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {_=console=>{},_=file_signal=>{}}
        })
        .await?;
    pool.close().await;
    drop(runtime_guard);
    Ok(())
}

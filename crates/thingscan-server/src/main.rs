//! `thingscan-server`: runs the account server, or manages its accounts.
//!
//! ```text
//! thingscan-server                      serve (THINGSCAN_ADDR, THINGSCAN_DB, THINGSCAN_ALLOW_SIGNUP)
//! thingscan-server list-users
//! thingscan-server delete-user <email>
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use thingscan_server::{Server, router};

fn main() -> ExitCode {
    let db = PathBuf::from(std::env::var("THINGSCAN_DB").unwrap_or_else(|_| "thingscan-server.db".into()));
    let allow_signup = std::env::var("THINGSCAN_ALLOW_SIGNUP")
        .map(|v| !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "no" | "off"))
        .unwrap_or(true);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let open = || match Server::open(&db, allow_signup) {
        Ok(server) => Some(server),
        Err(e) => {
            eprintln!("thingscan-server: can't open {}: {e}", db.display());
            None
        }
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["serve"] => {
            let Some(server) = open() else { return ExitCode::FAILURE };
            serve(server, &db, allow_signup)
        }
        ["list-users"] => {
            let Some(server) = open() else { return ExitCode::FAILURE };
            match server.list_users() {
                Ok(users) => {
                    for (email, created, synced) in users {
                        let synced = synced.map_or("never synced".to_string(), |t| format!("vault updated {t}"));
                        println!("{email}\tcreated {created}\t{synced}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("thingscan-server: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        ["delete-user", email] => {
            let Some(server) = open() else { return ExitCode::FAILURE };
            match server.delete_user(email) {
                Ok(true) => {
                    println!("Deleted {email}, its sessions and its vault.");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    eprintln!("No account with the email {email}.");
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("thingscan-server: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: thingscan-server [serve | list-users | delete-user <email>]");
            ExitCode::FAILURE
        }
    }
}

fn serve(server: Server, db: &std::path::Path, allow_signup: bool) -> ExitCode {
    let addr: SocketAddr = match std::env::var("THINGSCAN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into()).parse() {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("thingscan-server: bad THINGSCAN_ADDR: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = tokio::runtime::Runtime::new().expect("tokio starts");
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!("thingscan-server: can't listen on {addr}: {e}");
                return ExitCode::FAILURE;
            }
        };
        eprintln!(
            "thingscan-server: listening on {addr}, data in {}, new accounts {}",
            db.display(),
            if allow_signup { "allowed" } else { "off" }
        );
        let stopped = axum::serve(listener, router(Arc::new(server))).with_graceful_shutdown(async {
            let ctrl_c = tokio::signal::ctrl_c();
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
            tokio::select! {
                _ = ctrl_c => {}
                _ = term.recv() => {}
            }
        });
        match stopped.await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("thingscan-server: {e}");
                ExitCode::FAILURE
            }
        }
    })
}

//! `faber-agent` — the daemon half of agent transport
//! (`internal-docs/agent-transport.md`). Two commands, hand-rolled
//! rather than pulled in via `clap`: two subcommands and three flags total
//! doesn't earn a dependency the rest of the workspace doesn't otherwise
//! need.

mod config;
mod dial;
mod enroll;
mod exec;
mod handler;
mod service;
mod sftp;
mod ws_stream;

use config::{Config, Scope};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "faber_agent=info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("install") => {
            let install = match parse_install_args(&args[1..]) {
                Ok(parsed) => parsed,
                Err(message) => {
                    eprintln!("{message}");
                    std::process::exit(2);
                }
            };
            if let Err(error) =
                enroll::install(&install.api, &install.token, install.scope, install.start).await
            {
                eprintln!("install failed: {error}");
                std::process::exit(1);
            }
        }
        Some("run") => {
            let scope = match parse_run_args(&args[1..]) {
                Ok(scope) => scope,
                Err(message) => {
                    eprintln!("{message}");
                    std::process::exit(2);
                }
            };
            let config = match Config::load(scope) {
                Ok(config) => config,
                Err(error) => {
                    eprintln!(
                        "could not read the daemon's config ({error}); run `faber-agent install` first"
                    );
                    std::process::exit(1);
                }
            };
            dial::run_forever(&config).await;
        }
        _ => {
            eprintln!(
                "usage:\n  \
                 faber-agent install --token <bootstrap-token> --api <https://faber.example.com> [--system] [--no-start]\n  \
                 faber-agent run [--system]"
            );
            std::process::exit(2);
        }
    }
}

struct Install {
    token: String,
    api: String,
    /// Which manager supervises the daemon, and therefore what authority it
    /// runs with. Fixed here and never renegotiated — see `service.rs`.
    scope: Scope,
    /// Whether to enable and start the systemd unit, rather than only
    /// writing it. Installing is the point of the command, so starting is
    /// the default and `--no-start` is the escape for anyone who supervises
    /// this some other way.
    start: bool,
}

fn parse_install_args(args: &[String]) -> Result<Install, String> {
    let mut token = None;
    let mut api = None;
    let mut scope = Scope::User;
    let mut start = true;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--token" => token = iter.next().cloned(),
            "--api" => api = iter.next().cloned(),
            "--system" => scope = Scope::System,
            "--no-start" => start = false,
            other => return Err(format!("unrecognized argument '{other}'")),
        }
    }
    let token = token.ok_or("--token is required")?;
    let api = api.ok_or("--api is required")?;
    Ok(Install {
        token,
        api,
        scope,
        start,
    })
}

/// `run` carries the same scope flag the unit was written with, because the
/// scope decides where the daemon's identity lives and nothing about the
/// running process reveals which install produced it.
fn parse_run_args(args: &[String]) -> Result<Scope, String> {
    let mut scope = Scope::User;
    for flag in args {
        match flag.as_str() {
            "--system" => scope = Scope::System,
            other => return Err(format!("unrecognized argument '{other}'")),
        }
    }
    Ok(scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn install_starts_the_service_unless_told_not_to() {
        let parsed = parse_install_args(&args(&["--token", "t", "--api", "https://f"])).unwrap();
        assert!(parsed.start);
        let parsed =
            parse_install_args(&args(&["--token", "t", "--api", "https://f", "--no-start"]))
                .unwrap();
        assert!(!parsed.start);
    }

    #[test]
    fn the_scope_is_stated_and_never_inferred() {
        let parsed = parse_install_args(&args(&["--token", "t", "--api", "https://f"])).unwrap();
        assert_eq!(parsed.scope, Scope::User);
        let parsed =
            parse_install_args(&args(&["--token", "t", "--api", "https://f", "--system"])).unwrap();
        assert_eq!(parsed.scope, Scope::System);
        // `run` has to be told the same thing: the config it reads lives in
        // a different place, and nothing about the process says which.
        assert_eq!(parse_run_args(&args(&[])).unwrap(), Scope::User);
        assert_eq!(parse_run_args(&args(&["--system"])).unwrap(), Scope::System);
    }

    #[test]
    fn install_refuses_to_guess_a_missing_flag() {
        assert!(parse_install_args(&args(&["--token", "t"])).is_err());
        assert!(parse_install_args(&args(&["--api", "https://f"])).is_err());
    }
}

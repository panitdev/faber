//! Connect-time resolution of a bound presentation target.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use environment::docker::engine;
use proxies::{Dial, Stream, TcpDialer};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    environments::reach_daemon,
    error::{ApiResult, AppError},
    models::{
        host::{ExecMode, Host, HostContainer, Transport},
        session::SessionEnvironment,
    },
    schema::{host, host_container},
    state::AppState,
};

const ADDRESS_TTL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    container_id: Uuid,
    network: Option<String>,
}

#[derive(Clone)]
struct CachedAddress {
    address: String,
    inserted: Instant,
}

#[derive(Default)]
pub struct ContainerAddressCache {
    entries: Mutex<HashMap<CacheKey, CachedAddress>>,
}

impl ContainerAddressCache {
    async fn get(&self, key: &CacheKey) -> Option<String> {
        let mut entries = self.entries.lock().await;
        match entries.get(key) {
            Some(entry) if entry.inserted.elapsed() < ADDRESS_TTL => Some(entry.address.clone()),
            Some(_) => {
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    async fn put(&self, key: CacheKey, address: String) {
        self.entries.lock().await.insert(
            key,
            CachedAddress {
                address,
                inserted: Instant::now(),
            },
        );
    }

    pub async fn invalidate(&self, container_id: Uuid, network: Option<&str>) {
        self.entries.lock().await.remove(&CacheKey {
            container_id,
            network: network.map(str::to_owned),
        });
    }
}

#[derive(Clone)]
pub enum PreviewDialer {
    Local,
    Ssh(Arc<environment::ssh::SshSession>),
}

#[async_trait]
impl Dial for PreviewDialer {
    type Connection = Box<dyn Stream>;

    async fn dial(&self, address: &str) -> Result<Self::Connection, proxies::Error> {
        match self {
            Self::Local => TcpDialer
                .dial(address)
                .await
                .map(|stream| Box::new(stream) as Box<dyn Stream>),
            Self::Ssh(session) => {
                let (host, port) =
                    split_address(address).map_err(|source| proxies::Error::Dial {
                        address: address.to_owned(),
                        source,
                    })?;
                session
                    .open_tcp(host, port)
                    .await
                    .map(|stream| Box::new(stream) as Box<dyn Stream>)
                    .map_err(|error| proxies::Error::Dial {
                        address: address.to_owned(),
                        source: std::io::Error::other(error.to_string()),
                    })
            }
        }
    }
}

pub struct ResolvedEndpoint {
    pub address: String,
    pub dialer: PreviewDialer,
    pub container_cache: Option<(Uuid, Option<String>)>,
}

pub async fn endpoint(
    state: &AppState,
    binding: &SessionEnvironment,
    port: u16,
    bypass_cache: bool,
) -> ApiResult<ResolvedEndpoint> {
    let mut conn = state.db.get().await?;
    let found: Host = host::table
        .filter(host::id.eq(binding.host_id))
        .select(Host::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(|error| AppError::db(error, "presentation.resolve.host"))?
        .ok_or_else(|| AppError::BadGateway("the bound host no longer exists".into()))?;
    if found.disabled_at.is_some() {
        return Err(AppError::BadGateway("the bound host is disabled".into()));
    }

    let container = match binding.container_id {
        Some(id) => Some(
            host_container::table
                .filter(host_container::id.eq(id))
                .filter(host_container::host_id.eq(found.id))
                .select(HostContainer::as_select())
                .first(&mut conn)
                .await
                .optional()
                .map_err(|error| AppError::db(error, "presentation.resolve.container"))?
                .ok_or_else(|| {
                    AppError::BadGateway("the bound container no longer exists".into())
                })?,
        ),
        None => None,
    };
    drop(conn);

    let user_id = container
        .as_ref()
        .map(|container| container.user_id)
        .unwrap_or(found.user_id);
    let dialer = match found.transport.as_str() {
        value if value == Transport::Local.as_str() => PreviewDialer::Local,
        value if value == Transport::Agent.as_str() => {
            PreviewDialer::Ssh(state.agents.get(found.id).ok_or_else(|| {
                AppError::BadGateway(format!("no agent daemon is connected for '{}'", found.name))
            })?)
        }
        value if value == Transport::Ssh.as_str() => {
            PreviewDialer::Ssh(state.ssh.get(state, user_id, &found).await?)
        }
        _ => {
            return Err(AppError::BadGateway(
                "the bound host has an unknown transport".into(),
            ));
        }
    };

    if found.exec_mode == ExecMode::Direct.as_str() {
        if container.is_some() {
            return Err(AppError::BadGateway(
                "a direct host binding unexpectedly names a container".into(),
            ));
        }
        return Ok(ResolvedEndpoint {
            address: format!("127.0.0.1:{port}"),
            dialer,
            container_cache: None,
        });
    }

    let container = container.ok_or_else(|| {
        AppError::BadGateway("a Docker presentation has no bound container".into())
    })?;
    if container.unregistered_at.is_some() {
        return Err(AppError::BadGateway(
            "the bound container is unregistered".into(),
        ));
    }
    let key = CacheKey {
        container_id: container.id,
        network: found.preview_network.clone(),
    };
    let cached = if bypass_cache {
        None
    } else {
        state.presentation_addresses.get(&key).await
    };
    let address = match cached {
        Some(address) => address,
        None => {
            let daemon = reach_daemon(state, user_id, &found).await?;
            let networks = engine::container_networks(&daemon, &container.container_ref)
                .await
                .map_err(|error| AppError::BadGateway(error.to_string()))?;
            let address = select_address(&networks, found.preview_network.as_deref())?;
            state.presentation_addresses.put(key, address.clone()).await;
            address
        }
    };

    Ok(ResolvedEndpoint {
        address: format!("{address}:{port}"),
        dialer,
        container_cache: Some((container.id, found.preview_network)),
    })
}

fn select_address(
    networks: &engine::ContainerNetworks,
    configured: Option<&str>,
) -> ApiResult<String> {
    match networks.mode.as_str() {
        "host" => return Ok("127.0.0.1".into()),
        "none" => {
            return Err(AppError::BadGateway(
                "the container has networking disabled".into(),
            ));
        }
        mode if mode.starts_with("container:") => {
            return Err(AppError::BadGateway(
                "container-shared network namespaces are not supported for previews".into(),
            ));
        }
        _ => {}
    }
    if let Some(name) = configured {
        return networks
            .addresses
            .iter()
            .find(|(network, _)| network == name)
            .map(|(_, address)| address.clone())
            .ok_or_else(|| {
                AppError::BadGateway(format!(
                    "the container has no address on configured preview network '{name}'"
                ))
            });
    }
    match networks.addresses.as_slice() {
        [(_, address)] => Ok(address.clone()),
        [] => Err(AppError::BadGateway(
            "the container has no network address".into(),
        )),
        _ => Err(AppError::BadGateway(
            "the container has multiple networks; configure host.preview_network".into(),
        )),
    }
}

fn split_address(address: &str) -> Result<(&str, u16), std::io::Error> {
    let (host, port) = address.rsplit_once(':').ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "address has no port")
    })?;
    let port = port.parse().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "address has an invalid port",
        )
    })?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_network_wins_and_ambiguity_refuses() {
        let networks = engine::ContainerNetworks {
            mode: "default".into(),
            addresses: vec![
                ("a".into(), "10.0.0.2".into()),
                ("b".into(), "10.1.0.2".into()),
            ],
        };
        assert_eq!(select_address(&networks, Some("b")).unwrap(), "10.1.0.2");
        assert!(select_address(&networks, None).is_err());
    }

    #[test]
    fn host_network_uses_remote_loopback() {
        let networks = engine::ContainerNetworks {
            mode: "host".into(),
            addresses: Vec::new(),
        };
        assert_eq!(select_address(&networks, None).unwrap(), "127.0.0.1");
    }
}

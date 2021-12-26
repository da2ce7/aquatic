use std::net::SocketAddr;
use std::path::PathBuf;

#[cfg(feature = "cpu-pinning")]
use aquatic_common::cpu_pinning::CpuPinningConfig;
use aquatic_common::{access_list::AccessListConfig, privileges::PrivilegeConfig};
use serde::Deserialize;

use aquatic_cli_helpers::LogLevel;
use toml_config::TomlConfig;

/// aquatic_ws configuration
#[derive(Clone, Debug, PartialEq, TomlConfig, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Socket workers receive requests from the socket, parse them and send
    /// them on to the request workers. They then receive responses from the
    /// request workers, encode them and send them back over the socket.
    pub socket_workers: usize,
    /// Request workers receive a number of requests from socket workers,
    /// generate responses and send them back to the socket workers.
    pub request_workers: usize,
    pub log_level: LogLevel,
    pub network: NetworkConfig,
    pub protocol: ProtocolConfig,
    #[cfg(feature = "with-mio")]
    pub handlers: HandlerConfig,
    pub cleaning: CleaningConfig,
    pub privileges: PrivilegeConfig,
    pub access_list: AccessListConfig,
    #[cfg(feature = "cpu-pinning")]
    pub cpu_pinning: CpuPinningConfig,
    #[cfg(feature = "with-mio")]
    pub statistics: StatisticsConfig,
}

impl aquatic_cli_helpers::Config for Config {
    fn get_log_level(&self) -> Option<LogLevel> {
        Some(self.log_level)
    }
}

#[derive(Clone, Debug, PartialEq, TomlConfig, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Bind to this address
    pub address: SocketAddr,
    /// Only allow access over IPv6
    pub ipv6_only: bool,

    /// Path to TLS certificate (DER-encoded X.509)
    pub tls_certificate_path: PathBuf,
    /// Path to TLS private key (DER-encoded ASN.1 in PKCS#8 or PKCS#1 format)
    pub tls_private_key_path: PathBuf,

    pub websocket_max_message_size: usize,
    pub websocket_max_frame_size: usize,

    #[cfg(feature = "with-mio")]
    pub poll_event_capacity: usize,
    #[cfg(feature = "with-mio")]
    pub poll_timeout_microseconds: u64,
}

#[derive(Clone, Debug, PartialEq, TomlConfig, Deserialize)]
#[serde(default)]
pub struct ProtocolConfig {
    /// Maximum number of torrents to accept in scrape request
    pub max_scrape_torrents: usize,
    /// Maximum number of offers to accept in announce request
    pub max_offers: usize,
    /// Ask peers to announce this often (seconds)
    pub peer_announce_interval: usize,
}

#[cfg(feature = "with-mio")]
#[derive(Clone, Debug, PartialEq, TomlConfig, Deserialize)]
#[serde(default)]
pub struct HandlerConfig {
    /// Maximum number of requests to receive from channel before locking
    /// mutex and starting work
    pub max_requests_per_iter: usize,
    pub channel_recv_timeout_microseconds: u64,
}

#[derive(Clone, Debug, PartialEq, TomlConfig, Deserialize)]
#[serde(default)]
pub struct CleaningConfig {
    /// Clean peers this often (seconds)
    pub torrent_cleaning_interval: u64,
    /// Remove peers that have not announced for this long (seconds)
    pub max_peer_age: u64,

    // Clean connections this often (seconds)
    #[cfg(feature = "with-glommio")]
    pub connection_cleaning_interval: u64,
    /// Close connections if no responses have been sent to them for this long (seconds)
    #[cfg(feature = "with-glommio")]
    pub max_connection_idle: u64,

    /// Remove connections that are older than this (seconds)
    #[cfg(feature = "with-mio")]
    pub max_connection_age: u64,
}

#[cfg(feature = "with-mio")]
#[derive(Clone, Debug, PartialEq, TomlConfig, Deserialize)]
#[serde(default)]
pub struct StatisticsConfig {
    /// Print statistics this often (seconds). Do not print when set to zero.
    pub interval: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket_workers: 1,
            request_workers: 1,
            log_level: LogLevel::default(),
            network: NetworkConfig::default(),
            protocol: ProtocolConfig::default(),
            #[cfg(feature = "with-mio")]
            handlers: Default::default(),
            cleaning: CleaningConfig::default(),
            privileges: PrivilegeConfig::default(),
            access_list: AccessListConfig::default(),
            #[cfg(feature = "cpu-pinning")]
            cpu_pinning: Default::default(),
            #[cfg(feature = "with-mio")]
            statistics: Default::default(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            address: SocketAddr::from(([0, 0, 0, 0], 3000)),
            ipv6_only: false,

            tls_certificate_path: "".into(),
            tls_private_key_path: "".into(),

            websocket_max_message_size: 64 * 1024,
            websocket_max_frame_size: 16 * 1024,

            #[cfg(feature = "with-mio")]
            poll_event_capacity: 4096,
            #[cfg(feature = "with-mio")]
            poll_timeout_microseconds: 200_000,
        }
    }
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            max_scrape_torrents: 255,
            max_offers: 10,
            peer_announce_interval: 120,
        }
    }
}

#[cfg(feature = "with-mio")]
impl Default for HandlerConfig {
    fn default() -> Self {
        Self {
            max_requests_per_iter: 256,
            channel_recv_timeout_microseconds: 200,
        }
    }
}

impl Default for CleaningConfig {
    fn default() -> Self {
        Self {
            torrent_cleaning_interval: 30,
            max_peer_age: 1800,
            #[cfg(feature = "with-glommio")]
            max_connection_idle: 60 * 5,

            #[cfg(feature = "with-mio")]
            max_connection_age: 1800,
            #[cfg(feature = "with-glommio")]
            connection_cleaning_interval: 30,
        }
    }
}

#[cfg(feature = "with-mio")]
impl Default for StatisticsConfig {
    fn default() -> Self {
        Self { interval: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    ::toml_config::gen_serialize_deserialize_test!(Config);
}

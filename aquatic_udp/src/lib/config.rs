use std::net::SocketAddr;

use aquatic_common::{access_list::AccessListConfig, privileges::PrivilegeConfig};
use serde::{Deserialize, Serialize};

use aquatic_cli_helpers::LogLevel;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Socket workers receive requests from the socket, parse them and send
    /// them on to the request workers. They then recieve responses from the
    /// request workers, encode them and send them back over the socket.
    pub socket_workers: usize,
    /// Request workers receive a number of requests from socket workers,
    /// generate responses and send them back to the socket workers.
    pub request_workers: usize,
    pub log_level: LogLevel,
    pub network: NetworkConfig,
    pub protocol: ProtocolConfig,
    pub handlers: HandlerConfig,
    pub statistics: StatisticsConfig,
    pub cleaning: CleaningConfig,
    pub privileges: PrivilegeConfig,
    pub access_list: AccessListConfig,
    #[cfg(feature = "cpu-pinning")]
    pub cpu_pinning: aquatic_common::cpu_pinning::CpuPinningConfig,
}

impl aquatic_cli_helpers::Config for Config {
    fn get_log_level(&self) -> Option<LogLevel> {
        Some(self.log_level)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Bind to this address
    pub address: SocketAddr,
    /// Size of socket recv buffer. Use 0 for OS default.
    ///
    /// This setting can have a big impact on dropped packages. It might
    /// require changing system defaults. Some examples of commands to set
    /// recommended values for different operating systems:
    ///
    /// macOS:
    /// $ sudo sysctl net.inet.udp.recvspace=6000000
    /// $ sudo sysctl net.inet.udp.maxdgram=500000 # Not necessary, but recommended
    /// $ sudo sysctl kern.ipc.maxsockbuf=8388608 # Not necessary, but recommended
    ///
    /// Linux:
    /// $ sudo sysctl -w net.core.rmem_max=104857600
    /// $ sudo sysctl -w net.core.rmem_default=104857600
    pub socket_recv_buffer_size: usize,
    #[cfg(feature = "with-mio")]
    pub poll_event_capacity: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolConfig {
    /// Maximum number of torrents to accept in scrape request
    pub max_scrape_torrents: u8,
    /// Maximum number of peers to return in announce response
    pub max_response_peers: usize,
    /// Ask peers to announce this often (seconds)
    pub peer_announce_interval: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HandlerConfig {
    /// Maximum number of requests to receive from channel before locking
    /// mutex and starting work
    pub max_requests_per_iter: usize,
    pub channel_recv_timeout_microseconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StatisticsConfig {
    /// Print statistics this often (seconds). Don't print when set to zero.
    pub interval: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CleaningConfig {
    /// Clean connections this often (seconds)
    pub connection_cleaning_interval: u64,
    /// Clean torrents this often (seconds)
    pub torrent_cleaning_interval: u64,
    /// Remove connections that are older than this (seconds)
    pub max_connection_age: u64,
    /// Remove peers that haven't announced for this long (seconds)
    pub max_peer_age: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket_workers: 1,
            request_workers: 1,
            log_level: LogLevel::Error,
            network: NetworkConfig::default(),
            protocol: ProtocolConfig::default(),
            handlers: HandlerConfig::default(),
            statistics: StatisticsConfig::default(),
            cleaning: CleaningConfig::default(),
            privileges: PrivilegeConfig::default(),
            access_list: AccessListConfig::default(),
            #[cfg(feature = "cpu-pinning")]
            cpu_pinning: Default::default(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            address: SocketAddr::from(([0, 0, 0, 0], 3000)),
            socket_recv_buffer_size: 4096 * 128,
            #[cfg(feature = "with-mio")]
            poll_event_capacity: 4096,
        }
    }
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            max_scrape_torrents: 255,
            max_response_peers: 255,
            peer_announce_interval: 60 * 15,
        }
    }
}

impl Default for HandlerConfig {
    fn default() -> Self {
        Self {
            max_requests_per_iter: 10000,
            channel_recv_timeout_microseconds: 200,
        }
    }
}

impl Default for StatisticsConfig {
    fn default() -> Self {
        Self { interval: 0 }
    }
}

impl Default for CleaningConfig {
    fn default() -> Self {
        Self {
            connection_cleaning_interval: 60,
            torrent_cleaning_interval: 60 * 2,
            max_connection_age: 60 * 5,
            max_peer_age: 60 * 20,
        }
    }
}

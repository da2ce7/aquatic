use std::sync::atomic::Ordering;

use histogram::Histogram;

use super::common::*;
use crate::config::Config;

pub fn gather_and_print_statistics(state: &State, config: &Config) {
    let interval = config.statistics.interval;

    let requests_received: f64 = state
        .statistics
        .requests_received
        .fetch_and(0, Ordering::SeqCst) as f64;
    let responses_sent: f64 = state
        .statistics
        .responses_sent
        .fetch_and(0, Ordering::SeqCst) as f64;
    let bytes_received: f64 = state
        .statistics
        .bytes_received
        .fetch_and(0, Ordering::SeqCst) as f64;
    let bytes_sent: f64 = state.statistics.bytes_sent.fetch_and(0, Ordering::SeqCst) as f64;

    let requests_per_second = requests_received / interval as f64;
    let responses_per_second: f64 = responses_sent / interval as f64;
    let bytes_received_per_second: f64 = bytes_received / interval as f64;
    let bytes_sent_per_second: f64 = bytes_sent / interval as f64;

    println!(
        "stats: {:.2} requests/second, {:.2} responses/second",
        requests_per_second, responses_per_second
    );

    println!(
        "bandwidth: {:7.2} Mbit/s in, {:7.2} Mbit/s out",
        bytes_received_per_second * 8.0 / 1_000_000.0,
        bytes_sent_per_second * 8.0 / 1_000_000.0,
    );

    let mut total_num_torrents_ipv4 = 0usize;
    let mut total_num_torrents_ipv6 = 0usize;
    let mut total_num_peers_ipv4 = 0usize;
    let mut total_num_peers_ipv6 = 0usize;

    let mut peers_per_torrent = Histogram::new();

    {
        let torrents = &mut state.torrents.lock();

        for torrent in torrents.ipv4.values() {
            let num_peers = torrent.num_seeders + torrent.num_leechers;

            if let Err(err) = peers_per_torrent.increment(num_peers as u64) {
                ::log::error!("error incrementing peers_per_torrent histogram: {}", err)
            }

            total_num_peers_ipv4 += num_peers;
        }
        for torrent in torrents.ipv6.values() {
            let num_peers = torrent.num_seeders + torrent.num_leechers;

            if let Err(err) = peers_per_torrent.increment(num_peers as u64) {
                ::log::error!("error incrementing peers_per_torrent histogram: {}", err)
            }

            total_num_peers_ipv6 += num_peers;
        }

        total_num_torrents_ipv4 += torrents.ipv4.len();
        total_num_torrents_ipv6 += torrents.ipv6.len();
    }

    println!(
        "ipv4 torrents: {}, peers: {}; ipv6 torrents: {}, peers: {}",
        total_num_torrents_ipv4,
        total_num_peers_ipv4,
        total_num_torrents_ipv6,
        total_num_peers_ipv6,
    );

    if peers_per_torrent.entries() != 0 {
        println!(
            "peers per torrent: min: {}, p50: {}, p75: {}, p90: {}, p99: {}, p999: {}, max: {}",
            peers_per_torrent.minimum().unwrap(),
            peers_per_torrent.percentile(50.0).unwrap(),
            peers_per_torrent.percentile(75.0).unwrap(),
            peers_per_torrent.percentile(90.0).unwrap(),
            peers_per_torrent.percentile(99.0).unwrap(),
            peers_per_torrent.percentile(99.9).unwrap(),
            peers_per_torrent.maximum().unwrap(),
        );
    }

    println!();
}

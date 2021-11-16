use std::sync::Arc;

use rand::distributions::WeightedIndex;
use rand::prelude::*;
use rand_distr::Pareto;

use aquatic_udp_protocol::*;

use crate::common::*;
use crate::utils::*;

pub fn process_response(
    rng: &mut impl Rng,
    pareto: Pareto<f64>,
    info_hashes: &Arc<Vec<InfoHash>>,
    config: &Config,
    torrent_peers: &mut TorrentPeerMap,
    response: Response,
) -> Option<Request> {
    match response {
        Response::Connect(r) => {
            // Fetch the torrent peer or create it if is doesn't exists. Update
            // the connection id if fetched. Create a request and move the
            // torrent peer appropriately.

            let torrent_peer = torrent_peers
                .remove(&r.transaction_id)
                .map(|mut torrent_peer| {
                    torrent_peer.connection_id = r.connection_id;

                    torrent_peer
                })
                .unwrap_or_else(|| {
                    create_torrent_peer(config, rng, pareto, info_hashes, r.connection_id)
                });

            let new_transaction_id = generate_transaction_id(rng);

            let request =
                create_random_request(config, rng, info_hashes, new_transaction_id, &torrent_peer);

            torrent_peers.insert(new_transaction_id, torrent_peer);

            Some(request)
        }
        Response::AnnounceIpv4(r) => if_torrent_peer_move_and_create_random_request(
            config,
            rng,
            info_hashes,
            torrent_peers,
            r.transaction_id,
        ),
        Response::AnnounceIpv6(r) => if_torrent_peer_move_and_create_random_request(
            config,
            rng,
            info_hashes,
            torrent_peers,
            r.transaction_id,
        ),
        Response::Scrape(r) => if_torrent_peer_move_and_create_random_request(
            config,
            rng,
            info_hashes,
            torrent_peers,
            r.transaction_id,
        ),
        Response::Error(r) => {
            if !r.message.to_lowercase().contains("connection") {
                eprintln!(
                    "Received error response which didn't contain the word 'connection': {}",
                    r.message
                );
            }

            if let Some(torrent_peer) = torrent_peers.remove(&r.transaction_id) {
                let new_transaction_id = generate_transaction_id(rng);

                torrent_peers.insert(new_transaction_id, torrent_peer);

                Some(create_connect_request(new_transaction_id))
            } else {
                Some(create_connect_request(generate_transaction_id(rng)))
            }
        }
    }
}

fn if_torrent_peer_move_and_create_random_request(
    config: &Config,
    rng: &mut impl Rng,
    info_hashes: &Arc<Vec<InfoHash>>,
    torrent_peers: &mut TorrentPeerMap,
    transaction_id: TransactionId,
) -> Option<Request> {
    if let Some(torrent_peer) = torrent_peers.remove(&transaction_id) {
        let new_transaction_id = generate_transaction_id(rng);

        let request =
            create_random_request(config, rng, info_hashes, new_transaction_id, &torrent_peer);

        torrent_peers.insert(new_transaction_id, torrent_peer);

        Some(request)
    } else {
        None
    }
}

fn create_random_request(
    config: &Config,
    rng: &mut impl Rng,
    info_hashes: &Arc<Vec<InfoHash>>,
    transaction_id: TransactionId,
    torrent_peer: &TorrentPeer,
) -> Request {
    let weights = vec![
        config.handler.weight_announce as u32,
        config.handler.weight_connect as u32,
        config.handler.weight_scrape as u32,
    ];

    let items = vec![
        RequestType::Announce,
        RequestType::Connect,
        RequestType::Scrape,
    ];

    let dist = WeightedIndex::new(&weights).expect("random request weighted index");

    match items[dist.sample(rng)] {
        RequestType::Announce => create_announce_request(config, rng, torrent_peer, transaction_id),
        RequestType::Connect => create_connect_request(transaction_id),
        RequestType::Scrape => create_scrape_request(&info_hashes, torrent_peer, transaction_id),
    }
}

fn create_announce_request(
    config: &Config,
    rng: &mut impl Rng,
    torrent_peer: &TorrentPeer,
    transaction_id: TransactionId,
) -> Request {
    let (event, bytes_left) = {
        if rng.gen_bool(config.handler.peer_seeder_probability) {
            (AnnounceEvent::Completed, NumberOfBytes(0))
        } else {
            (AnnounceEvent::Started, NumberOfBytes(50))
        }
    };

    (AnnounceRequest {
        connection_id: torrent_peer.connection_id,
        transaction_id,
        info_hash: torrent_peer.info_hash,
        peer_id: torrent_peer.peer_id,
        bytes_downloaded: NumberOfBytes(50),
        bytes_uploaded: NumberOfBytes(50),
        bytes_left,
        event,
        ip_address: None,
        key: PeerKey(12345),
        peers_wanted: NumberOfPeers(100),
        port: torrent_peer.port,
    })
    .into()
}

fn create_scrape_request(
    info_hashes: &Arc<Vec<InfoHash>>,
    torrent_peer: &TorrentPeer,
    transaction_id: TransactionId,
) -> Request {
    let indeces = &torrent_peer.scrape_hash_indeces;

    let mut scape_hashes = Vec::with_capacity(indeces.len());

    for i in indeces {
        scape_hashes.push(info_hashes[*i].to_owned())
    }

    (ScrapeRequest {
        connection_id: torrent_peer.connection_id,
        transaction_id,
        info_hashes: scape_hashes,
    })
    .into()
}

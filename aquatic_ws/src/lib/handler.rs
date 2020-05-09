use std::time::Duration;
use std::vec::Drain;

use hashbrown::HashMap;
use parking_lot::MutexGuard;
use rand::{Rng, SeedableRng, rngs::SmallRng};

use aquatic::handlers::{extract_response_peers};

use crate::common::*;
use crate::protocol::*;


pub fn run_request_worker(
    state: State,
    in_message_receiver: InMessageReceiver,
    out_message_sender: OutMessageSender,
){
    let mut out_messages = Vec::new();

    let mut announce_requests = Vec::new();
    let mut scrape_requests = Vec::new();

    let mut rng = SmallRng::from_entropy();

    let timeout = Duration::from_micros(200);

    loop {
        let mut opt_torrent_map_guard: Option<MutexGuard<TorrentMap>> = None;

        for i in 0..1000 {
            let opt_in_message = if i == 0 {
                in_message_receiver.recv().ok()
            } else {
                in_message_receiver.recv_timeout(timeout).ok()
            };

            match opt_in_message {
                Some((meta, InMessage::AnnounceRequest(r))) => {
                    announce_requests.push((meta, r));
                },
                Some((meta, InMessage::ScrapeRequest(r))) => {
                    scrape_requests.push((meta, r));
                },
                None => {
                    if let Some(torrent_guard) = state.torrents.try_lock(){
                        opt_torrent_map_guard = Some(torrent_guard);

                        break
                    }
                }
            }
        }

        let mut torrent_map_guard = opt_torrent_map_guard
            .unwrap_or_else(|| state.torrents.lock());

        handle_announce_requests(
            &mut rng,
            &mut torrent_map_guard,
            &mut out_messages,
            announce_requests.drain(..)
        );

        handle_scrape_requests(
            &mut torrent_map_guard,
            &mut out_messages,
            scrape_requests.drain(..)
        );

        ::std::mem::drop(torrent_map_guard);

        for (meta, out_message) in out_messages.drain(..){
            out_message_sender.send(meta, out_message);
        }
    }
}


pub fn handle_announce_requests(
    rng: &mut impl Rng,
    torrents: &mut TorrentMap,
    messages_out: &mut Vec<(ConnectionMeta, OutMessage)>,
    requests: Drain<(ConnectionMeta, AnnounceRequest)>,
){
    let valid_until = ValidUntil::new(240);

    for (sender_meta, request) in requests {
        let torrent_data = torrents.entry(request.info_hash)
            .or_default();

        let peer_status = PeerStatus::from_event_and_bytes_left(
            request.event,
            request.bytes_left
        );

        let peer = Peer {
            connection_meta: sender_meta,
            status: peer_status,
            valid_until,
        };
        
        let opt_removed_peer = match peer_status {
            PeerStatus::Leeching => {
                torrent_data.num_leechers += 1;

                torrent_data.peers.insert(request.peer_id, peer)
            },
            PeerStatus::Seeding => {
                torrent_data.num_seeders += 1;

                torrent_data.peers.insert(request.peer_id, peer)
            },
            PeerStatus::Stopped => {
                torrent_data.peers.remove(&request.peer_id)
            }
        };

        match opt_removed_peer.map(|peer| peer.status){
            Some(PeerStatus::Leeching) => {
                torrent_data.num_leechers -= 1;
            },
            Some(PeerStatus::Seeding) => {
                torrent_data.num_seeders -= 1;
            },
            _ => {}
        }

        // If peer sent offers, send them on to random peers
        if let Some(offers) = request.offers {
            let max_num_peers_to_take = offers.len().min(10); // FIXME: config

            fn f(peer: &Peer) -> Peer {
                *peer
            }

            let peers = extract_response_peers(
                rng,
                &torrent_data.peers,
                max_num_peers_to_take,
                f
            );

            for (offer, peer) in offers.into_iter().zip(peers){
                let middleman_offer = MiddlemanOfferToPeer {
                    info_hash: request.info_hash,
                    peer_id: request.peer_id,
                    offer: offer.offer,
                    offer_id: offer.offer_id,
                };

                messages_out.push((
                    peer.connection_meta,
                    OutMessage::Offer(middleman_offer)
                ));
            }
        }

        // If peer sent answer, send it on to relevant peer
        match (request.answer, request.to_peer_id, request.offer_id){
            (Some(answer), Some(to_peer_id), Some(offer_id)) => {
                if let Some(to_peer) = torrent_data.peers.get(&to_peer_id){
                    let middleman_answer = MiddlemanAnswerToPeer {
                        peer_id: request.peer_id,
                        info_hash: request.info_hash,
                        answer,
                        offer_id,
                    };

                    messages_out.push((
                        to_peer.connection_meta,
                        OutMessage::Answer(middleman_answer)
                    ));
                }
            },
            _ => (),
        }

        let response = OutMessage::AnnounceResponse(AnnounceResponse {
            info_hash: request.info_hash,
            complete: torrent_data.num_seeders,
            incomplete: torrent_data.num_leechers,
            announce_interval: 120, // FIXME: config
        });

        messages_out.push((sender_meta, response));
    }
}


pub fn handle_scrape_requests(
    torrents: &mut TorrentMap,
    messages_out: &mut Vec<(ConnectionMeta, OutMessage)>,
    requests: Drain<(ConnectionMeta, ScrapeRequest)>,
){
    messages_out.extend(requests.map(|(meta, request)| {
        let num_info_hashes = request.info_hashes.len();

        let mut response = ScrapeResponse {
            files: HashMap::with_capacity(num_info_hashes),
        };

        // If request.info_hashes is empty, don't return scrape for all
        // torrents, even though reference server does it. It is too expensive.
        for info_hash in request.info_hashes {
            if let Some(torrent_data) = torrents.get(&info_hash){
                let stats = ScrapeStatistics {
                    complete: torrent_data.num_seeders,
                    downloaded: 0, // No implementation planned
                    incomplete: torrent_data.num_leechers,
                };

                response.files.insert(info_hash, stats);
            }
        }

        (meta, OutMessage::ScrapeResponse(response))
    }));
}
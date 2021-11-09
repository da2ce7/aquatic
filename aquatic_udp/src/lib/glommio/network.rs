use std::cell::RefCell;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use aquatic_common::access_list::create_access_list_cache;
use aquatic_common::AHashIndexMap;
use futures_lite::{Stream, StreamExt};
use glommio::channels::channel_mesh::{MeshBuilder, Partial, Role, Senders};
use glommio::channels::local_channel::{new_unbounded, LocalSender};
use glommio::enclose;
use glommio::net::UdpSocket;
use glommio::prelude::*;
use glommio::timer::TimerActionRepeat;
use rand::prelude::{Rng, SeedableRng, StdRng};

use aquatic_udp_protocol::{Request, Response};

use super::common::State;

use crate::common::handlers::*;
use crate::common::network::{ConnectionMap, ResponseSender};
use crate::common::*;
use crate::config::Config;

const PENDING_SCRAPE_MAX_WAIT: u64 = 30;

struct PendingScrapeResponse {
    pending_worker_responses: usize,
    valid_until: ValidUntil,
    stats: BTreeMap<usize, TorrentScrapeStatistics>,
}

#[derive(Default)]
struct PendingScrapeResponses(AHashIndexMap<TransactionId, PendingScrapeResponse>);

impl PendingScrapeResponses {
    fn prepare(
        &mut self,
        transaction_id: TransactionId,
        pending_worker_responses: usize,
        valid_until: ValidUntil,
    ) {
        let pending = PendingScrapeResponse {
            pending_worker_responses,
            valid_until,
            stats: BTreeMap::new(),
        };

        self.0.insert(transaction_id, pending);
    }

    fn add_and_get_finished(
        &mut self,
        mut response: ScrapeResponse,
        mut original_indices: Vec<usize>,
    ) -> Option<ScrapeResponse> {
        let finished = if let Some(r) = self.0.get_mut(&response.transaction_id) {
            r.pending_worker_responses -= 1;

            r.stats.extend(
                original_indices
                    .drain(..)
                    .zip(response.torrent_stats.drain(..)),
            );

            r.pending_worker_responses == 0
        } else {
            ::log::warn!("PendingScrapeResponses.add didn't find PendingScrapeResponse in map");

            false
        };

        if finished {
            let PendingScrapeResponse { stats, .. } =
                self.0.remove(&response.transaction_id).unwrap();

            Some(ScrapeResponse {
                transaction_id: response.transaction_id,
                torrent_stats: stats.into_values().collect(),
            })
        } else {
            None
        }
    }

    fn clean(&mut self) {
        let now = Instant::now();

        self.0.retain(|_, v| v.valid_until.0 > now);
        self.0.shrink_to_fit();
    }
}

pub async fn run_socket_worker(
    config: Config,
    state: State,
    request_mesh_builder: MeshBuilder<(usize, ConnectedRequest, SocketAddr), Partial>,
    response_mesh_builder: MeshBuilder<(ConnectedResponse, SocketAddr), Partial>,
    num_bound_sockets: Arc<AtomicUsize>,
) {
    let mut socket = UdpSocket::bind(config.network.address).unwrap();

    let recv_buffer_size = config.network.socket_recv_buffer_size;

    if recv_buffer_size != 0 {
        socket.set_buffer_size(recv_buffer_size);
    }

    let socket = Rc::new(socket);

    num_bound_sockets.fetch_add(1, Ordering::SeqCst);

    let (request_senders, _) = request_mesh_builder.join(Role::Producer).await.unwrap();
    let (_, mut response_receivers) = response_mesh_builder.join(Role::Consumer).await.unwrap();

    let response_consumer_index = response_receivers.consumer_id().unwrap();

    let (local_sender, local_receiver) = new_unbounded();
    let local_sender = Rc::new(local_sender);

    let pending_scrape_responses = Rc::new(RefCell::new(PendingScrapeResponses::default()));
    let response_sender = Rc::new(RefCell::new(ResponseSender::default()));

    // Periodically clean pending_scrape_responses
    TimerActionRepeat::repeat(enclose!((pending_scrape_responses) move || {
        enclose!((pending_scrape_responses) move || async move {
            pending_scrape_responses.borrow_mut().clean();

            Some(Duration::from_secs(120))
        })()
    }));

    // Periodically force sending of pending responses
    TimerActionRepeat::repeat(enclose!((config, socket, response_sender) move || {
        enclose!((config, socket, response_sender) move || async move {
            response_sender.borrow_mut().flush(socket.as_ref());

            Some(Duration::from_millis(config.network.response_buffer_max_pending_time_ms))
        })()
    }));

    spawn_local(
        enclose!((local_sender, pending_scrape_responses) read_requests(
            config.clone(),
            state,
            request_senders,
            response_consumer_index,
            local_sender,
            socket.clone(),
            pending_scrape_responses,
        )),
    )
    .detach();

    for (_, receiver) in response_receivers.streams().into_iter() {
        spawn_local(
            enclose!((local_sender, pending_scrape_responses) handle_shared_responses(
                local_sender,
                pending_scrape_responses,
                receiver,
            )),
        )
        .detach();
    }

    while let Some((response, addr)) = local_receiver.stream().next().await {
        response_sender
            .borrow_mut()
            .queue_and_maybe_send_response::<UdpSocket>(&socket, response, addr);

        yield_if_needed().await;
    }
}

async fn read_requests(
    config: Config,
    state: State,
    request_senders: Senders<(usize, ConnectedRequest, SocketAddr)>,
    response_consumer_index: usize,
    local_sender: Rc<LocalSender<(Response, SocketAddr)>>,
    socket: Rc<UdpSocket>,
    pending_scrape_responses: Rc<RefCell<PendingScrapeResponses>>,
) {
    let mut rng = StdRng::from_entropy();

    let access_list_mode = config.access_list.mode;

    let max_connection_age = config.cleaning.max_connection_age;
    let connection_valid_until = Rc::new(RefCell::new(ValidUntil::new(max_connection_age)));
    let pending_scrape_valid_until =
        Rc::new(RefCell::new(ValidUntil::new(PENDING_SCRAPE_MAX_WAIT)));
    let connections = Rc::new(RefCell::new(ConnectionMap::default()));
    let mut access_list_cache = create_access_list_cache(&state.access_list);

    // Periodically update connection_valid_until
    TimerActionRepeat::repeat(enclose!((connection_valid_until) move || {
        enclose!((connection_valid_until) move || async move {
            *connection_valid_until.borrow_mut() = ValidUntil::new(max_connection_age);

            Some(Duration::from_secs(1))
        })()
    }));

    // Periodically update pending_scrape_valid_until
    TimerActionRepeat::repeat(enclose!((pending_scrape_valid_until) move || {
        enclose!((pending_scrape_valid_until) move || async move {
            *pending_scrape_valid_until.borrow_mut() = ValidUntil::new(PENDING_SCRAPE_MAX_WAIT);

            Some(Duration::from_secs(10))
        })()
    }));

    // Periodically clean connections
    TimerActionRepeat::repeat(enclose!((config, connections) move || {
        enclose!((config, connections) move || async move {
            connections.borrow_mut().clean();

            Some(Duration::from_secs(config.cleaning.connection_cleaning_interval))
        })()
    }));

    let mut buf = [0u8; MAX_PACKET_SIZE];

    loop {
        match socket.recv_from(&mut buf).await {
            Ok((amt, src)) => {
                let request = Request::from_bytes(&buf[..amt], config.protocol.max_scrape_torrents);

                ::log::debug!("read request: {:?}", request);

                match request {
                    Ok(Request::Connect(request)) => {
                        let connection_id = ConnectionId(rng.gen());

                        connections.borrow_mut().insert(
                            connection_id,
                            src,
                            connection_valid_until.borrow().to_owned(),
                        );

                        let response = Response::Connect(ConnectResponse {
                            connection_id,
                            transaction_id: request.transaction_id,
                        });

                        local_sender.try_send((response, src)).unwrap();
                    }
                    Ok(Request::Announce(request)) => {
                        if connections.borrow().contains(request.connection_id, src) {
                            if access_list_cache
                                .load()
                                .allows(access_list_mode, &request.info_hash.0)
                            {
                                let request_consumer_index =
                                    calculate_request_consumer_index(&config, request.info_hash);

                                if let Err(err) = request_senders
                                    .send_to(
                                        request_consumer_index,
                                        (
                                            response_consumer_index,
                                            ConnectedRequest::Announce(request),
                                            src,
                                        ),
                                    )
                                    .await
                                {
                                    ::log::error!("request_sender.try_send failed: {:?}", err)
                                }
                            } else {
                                let response = Response::Error(ErrorResponse {
                                    transaction_id: request.transaction_id,
                                    message: "Info hash not allowed".into(),
                                });

                                local_sender.try_send((response, src)).unwrap();
                            }
                        }
                    }
                    Ok(Request::Scrape(ScrapeRequest {
                        transaction_id,
                        connection_id,
                        info_hashes,
                    })) => {
                        if connections.borrow().contains(connection_id, src) {
                            let mut consumer_requests: AHashIndexMap<
                                usize,
                                (ScrapeRequest, Vec<usize>),
                            > = Default::default();

                            for (i, info_hash) in info_hashes.into_iter().enumerate() {
                                let (req, indices) = consumer_requests
                                    .entry(calculate_request_consumer_index(&config, info_hash))
                                    .or_insert_with(|| {
                                        let request = ScrapeRequest {
                                            transaction_id: transaction_id,
                                            connection_id: connection_id,
                                            info_hashes: Vec::new(),
                                        };

                                        (request, Vec::new())
                                    });

                                req.info_hashes.push(info_hash);
                                indices.push(i);
                            }

                            pending_scrape_responses.borrow_mut().prepare(
                                transaction_id,
                                consumer_requests.len(),
                                pending_scrape_valid_until.borrow().to_owned(),
                            );

                            for (consumer_index, (request, original_indices)) in consumer_requests {
                                let request = ConnectedRequest::Scrape {
                                    request,
                                    original_indices,
                                };

                                if let Err(err) = request_senders
                                    .send_to(
                                        consumer_index,
                                        (response_consumer_index, request, src),
                                    )
                                    .await
                                {
                                    ::log::error!("request_sender.send failed: {:?}", err)
                                }
                            }
                        }
                    }
                    Err(err) => {
                        ::log::debug!("Request::from_bytes error: {:?}", err);

                        if let RequestParseError::Sendable {
                            connection_id,
                            transaction_id,
                            err,
                        } = err
                        {
                            if connections.borrow().contains(connection_id, src) {
                                let response = ErrorResponse {
                                    transaction_id,
                                    message: err.right_or("Parse error").into(),
                                };

                                local_sender.try_send((response.into(), src)).unwrap();
                            }
                        }
                    }
                }
            }
            Err(err) => {
                ::log::error!("recv_from: {:?}", err);
            }
        }

        yield_if_needed().await;
    }
}

async fn handle_shared_responses<S>(
    local_sender: Rc<LocalSender<(Response, SocketAddr)>>,
    pending_scrape_responses: Rc<RefCell<PendingScrapeResponses>>,
    mut stream: S,
) where
    S: Stream<Item = (ConnectedResponse, SocketAddr)> + ::std::marker::Unpin,
{
    while let Some((response, addr)) = stream.next().await {
        let opt_response = match response {
            ConnectedResponse::Announce(response) => Some((Response::Announce(response), addr)),
            ConnectedResponse::Scrape {
                response,
                original_indices,
            } => pending_scrape_responses
                .borrow_mut()
                .add_and_get_finished(response, original_indices)
                .map(|response| (Response::Scrape(response), addr)),
        };

        if let Some((response, addr)) = opt_response {
            local_sender.send((response, addr)).await;
        }

        yield_if_needed().await;
    }
}

fn calculate_request_consumer_index(config: &Config, info_hash: InfoHash) -> usize {
    (info_hash.0[0] as usize) % config.request_workers
}

use std::io::{Cursor, ErrorKind};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use aquatic_common::access_list::AccessListCache;
use aquatic_common::ServerStartInstant;
use crossbeam_channel::Receiver;
use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};

use aquatic_common::{
    access_list::create_access_list_cache, privileges::PrivilegeDropper, CanonicalSocketAddr,
    PanicSentinel, ValidUntil,
};
use aquatic_udp_protocol::*;

use crate::common::*;
use crate::config::Config;

use super::storage::PendingScrapeResponseSlab;
use super::validator::ConnectionValidator;
use super::{create_socket, EXTRA_PACKET_SIZE_IPV4, EXTRA_PACKET_SIZE_IPV6};

pub struct SocketWorker {
    config: Config,
    shared_state: State,
    request_sender: ConnectedRequestSender,
    response_receiver: Receiver<(ConnectedResponse, CanonicalSocketAddr)>,
    access_list_cache: AccessListCache,
    validator: ConnectionValidator,
    server_start_instant: ServerStartInstant,
    pending_scrape_responses: PendingScrapeResponseSlab,
    socket: UdpSocket,
    buffer: [u8; BUFFER_SIZE],
}

impl SocketWorker {
    pub fn run(
        _sentinel: PanicSentinel,
        shared_state: State,
        config: Config,
        validator: ConnectionValidator,
        server_start_instant: ServerStartInstant,
        request_sender: ConnectedRequestSender,
        response_receiver: Receiver<(ConnectedResponse, CanonicalSocketAddr)>,
        priv_dropper: PrivilegeDropper,
    ) {
        let socket =
            UdpSocket::from_std(create_socket(&config, priv_dropper).expect("create socket"));
        let access_list_cache = create_access_list_cache(&shared_state.access_list);

        let mut worker = Self {
            config,
            shared_state,
            validator,
            server_start_instant,
            request_sender,
            response_receiver,
            access_list_cache,
            pending_scrape_responses: Default::default(),
            socket,
            buffer: [0; BUFFER_SIZE],
        };

        worker.run_inner();
    }

    pub fn run_inner(&mut self) {
        let mut local_responses = Vec::new();
        let mut opt_resend_buffer =
            (self.config.network.resend_buffer_max_len > 0).then_some(Vec::new());

        let mut events = Events::with_capacity(self.config.network.poll_event_capacity);
        let mut poll = Poll::new().expect("create poll");

        poll.registry()
            .register(&mut self.socket, Token(0), Interest::READABLE)
            .expect("register poll");

        let poll_timeout = Duration::from_millis(self.config.network.poll_timeout_ms);

        let pending_scrape_cleaning_duration =
            Duration::from_secs(self.config.cleaning.pending_scrape_cleaning_interval);

        let mut pending_scrape_valid_until = ValidUntil::new(
            self.server_start_instant,
            self.config.cleaning.max_pending_scrape_age,
        );
        let mut last_pending_scrape_cleaning = Instant::now();

        let mut iter_counter = 0usize;

        loop {
            poll.poll(&mut events, Some(poll_timeout))
                .expect("failed polling");

            for event in events.iter() {
                if event.is_readable() {
                    self.read_and_handle_requests(&mut local_responses, pending_scrape_valid_until);
                }
            }

            // If resend buffer is enabled, send any responses in it
            if let Some(resend_buffer) = opt_resend_buffer.as_mut() {
                for (response, addr) in resend_buffer.drain(..) {
                    Self::send_response(
                        &self.config,
                        &self.shared_state,
                        &mut self.socket,
                        &mut self.buffer,
                        &mut None,
                        response,
                        addr,
                    );
                }
            }

            // Send any connect and error responses generated by this socket worker
            for (response, addr) in local_responses.drain(..) {
                Self::send_response(
                    &self.config,
                    &self.shared_state,
                    &mut self.socket,
                    &mut self.buffer,
                    &mut opt_resend_buffer,
                    response,
                    addr,
                );
            }

            // Check channel for any responses generated by swarm workers
            for (response, addr) in self.response_receiver.try_iter() {
                let opt_response = match response {
                    ConnectedResponse::Scrape(r) => self
                        .pending_scrape_responses
                        .add_and_get_finished(r)
                        .map(Response::Scrape),
                    ConnectedResponse::AnnounceIpv4(r) => Some(Response::AnnounceIpv4(r)),
                    ConnectedResponse::AnnounceIpv6(r) => Some(Response::AnnounceIpv6(r)),
                };

                if let Some(response) = opt_response {
                    Self::send_response(
                        &self.config,
                        &self.shared_state,
                        &mut self.socket,
                        &mut self.buffer,
                        &mut opt_resend_buffer,
                        response,
                        addr,
                    );
                }
            }

            // Run periodic ValidUntil updates and state cleaning
            if iter_counter % 256 == 0 {
                let seconds_since_start = self.server_start_instant.seconds_elapsed();

                pending_scrape_valid_until = ValidUntil::new_with_now(
                    seconds_since_start,
                    self.config.cleaning.max_pending_scrape_age,
                );

                let now = Instant::now();

                if now > last_pending_scrape_cleaning + pending_scrape_cleaning_duration {
                    self.pending_scrape_responses.clean(seconds_since_start);

                    last_pending_scrape_cleaning = now;
                }
            }

            iter_counter = iter_counter.wrapping_add(1);
        }
    }

    fn read_and_handle_requests(
        &mut self,
        local_responses: &mut Vec<(Response, CanonicalSocketAddr)>,
        pending_scrape_valid_until: ValidUntil,
    ) {
        let mut requests_received_ipv4: usize = 0;
        let mut requests_received_ipv6: usize = 0;
        let mut bytes_received_ipv4: usize = 0;
        let mut bytes_received_ipv6 = 0;

        loop {
            match self.socket.recv_from(&mut self.buffer[..]) {
                Ok((bytes_read, src)) => {
                    if src.port() == 0 {
                        ::log::info!("Ignored request from {} because source port is zero", src);

                        continue;
                    }

                    let src = CanonicalSocketAddr::new(src);

                    ::log::trace!("received request bytes: {}", hex_slice(&self.buffer[..bytes_read]));

                    let request_parsable = match Request::from_bytes(
                        &self.buffer[..bytes_read],
                        self.config.protocol.max_scrape_torrents,
                    ) {
                        Ok(request) => {
                            self.handle_request(
                                local_responses,
                                pending_scrape_valid_until,
                                request,
                                src,
                            );

                            true
                        }
                        Err(err) => {
                            ::log::debug!("Request::from_bytes error: {:?}", err);

                            if let RequestParseError::Sendable {
                                connection_id,
                                transaction_id,
                                err,
                            } = err
                            {
                                if self.validator.connection_id_valid(src, connection_id) {
                                    let response = ErrorResponse {
                                        transaction_id,
                                        message: err.into(),
                                    };

                                    local_responses.push((response.into(), src));
                                }
                            }

                            false
                        }
                    };

                    // Update statistics for converted address
                    if src.is_ipv4() {
                        if request_parsable {
                            requests_received_ipv4 += 1;
                        }
                        bytes_received_ipv4 += bytes_read + EXTRA_PACKET_SIZE_IPV4;
                    } else {
                        if request_parsable {
                            requests_received_ipv6 += 1;
                        }
                        bytes_received_ipv6 += bytes_read + EXTRA_PACKET_SIZE_IPV6;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    break;
                }
                Err(err) => {
                    ::log::warn!("recv_from error: {:#}", err);
                }
            }
        }

        if self.config.statistics.active() {
            self.shared_state
                .statistics_ipv4
                .requests_received
                .fetch_add(requests_received_ipv4, Ordering::Relaxed);
            self.shared_state
                .statistics_ipv6
                .requests_received
                .fetch_add(requests_received_ipv6, Ordering::Relaxed);
            self.shared_state
                .statistics_ipv4
                .bytes_received
                .fetch_add(bytes_received_ipv4, Ordering::Relaxed);
            self.shared_state
                .statistics_ipv6
                .bytes_received
                .fetch_add(bytes_received_ipv6, Ordering::Relaxed);
        }
    }

    fn handle_request(
        &mut self,
        local_responses: &mut Vec<(Response, CanonicalSocketAddr)>,
        pending_scrape_valid_until: ValidUntil,
        request: Request,
        src: CanonicalSocketAddr,
    ) {
        let access_list_mode = self.config.access_list.mode;

        match request {
            Request::Connect(request) => {
                ::log::trace!("received {:?} from {:?}", request, src);

                let connection_id = self.validator.create_connection_id(src);

                let response = Response::Connect(ConnectResponse {
                    connection_id,
                    transaction_id: request.transaction_id,
                });

                local_responses.push((response, src))
            }
            Request::Announce(request) => {
                ::log::trace!("received {:?} from {:?}", request, src);

                if self
                    .validator
                    .connection_id_valid(src, request.connection_id)
                {
                    if self
                        .access_list_cache
                        .load()
                        .allows(access_list_mode, &request.info_hash.0)
                    {
                        let worker_index =
                            SwarmWorkerIndex::from_info_hash(&self.config, request.info_hash);

                        self.request_sender.try_send_to(
                            worker_index,
                            ConnectedRequest::Announce(request),
                            src,
                        );
                    } else {
                        let response = Response::Error(ErrorResponse {
                            transaction_id: request.transaction_id,
                            message: "Info hash not allowed".into(),
                        });

                        local_responses.push((response, src))
                    }
                }
            }
            Request::Scrape(request) => {
                ::log::trace!("received {:?} from {:?}", request, src);

                if self
                    .validator
                    .connection_id_valid(src, request.connection_id)
                {
                    let split_requests = self.pending_scrape_responses.prepare_split_requests(
                        &self.config,
                        request,
                        pending_scrape_valid_until,
                    );

                    for (swarm_worker_index, request) in split_requests {
                        self.request_sender.try_send_to(
                            swarm_worker_index,
                            ConnectedRequest::Scrape(request),
                            src,
                        );
                    }
                }
            }
        }
    }

    fn send_response(
        config: &Config,
        shared_state: &State,
        socket: &mut UdpSocket,
        buffer: &mut [u8],
        opt_resend_buffer: &mut Option<Vec<(Response, CanonicalSocketAddr)>>,
        response: Response,
        canonical_addr: CanonicalSocketAddr,
    ) {
        let mut cursor = Cursor::new(buffer);

        if let Err(err) = response.write(&mut cursor) {
            ::log::error!("Converting response to bytes failed: {:#}", err);

            return;
        }

        let bytes_written = cursor.position() as usize;

        let addr = if config.network.address.is_ipv4() {
            canonical_addr
                .get_ipv4()
                .expect("found peer ipv6 address while running bound to ipv4 address")
        } else {
            canonical_addr.get_ipv6_mapped()
        };

        ::log::trace!("sending {:?} to {}, bytes: {}", response, addr, hex_slice(&cursor.get_ref()[..bytes_written]));

        match socket.send_to(&cursor.get_ref()[..bytes_written], addr) {
            Ok(amt) if config.statistics.active() => {
                let stats = if canonical_addr.is_ipv4() {
                    let stats = &shared_state.statistics_ipv4;

                    stats
                        .bytes_sent
                        .fetch_add(amt + EXTRA_PACKET_SIZE_IPV4, Ordering::Relaxed);

                    stats
                } else {
                    let stats = &shared_state.statistics_ipv6;

                    stats
                        .bytes_sent
                        .fetch_add(amt + EXTRA_PACKET_SIZE_IPV6, Ordering::Relaxed);

                    stats
                };

                match response {
                    Response::Connect(_) => {
                        stats.responses_sent_connect.fetch_add(1, Ordering::Relaxed);
                    }
                    Response::AnnounceIpv4(_) | Response::AnnounceIpv6(_) => {
                        stats
                            .responses_sent_announce
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    Response::Scrape(_) => {
                        stats.responses_sent_scrape.fetch_add(1, Ordering::Relaxed);
                    }
                    Response::Error(_) => {
                        stats.responses_sent_error.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Ok(_) => (),
            Err(err) => match opt_resend_buffer.as_mut() {
                Some(resend_buffer)
                    if (err.raw_os_error() == Some(libc::ENOBUFS))
                        || (err.kind() == ErrorKind::WouldBlock) =>
                {
                    if resend_buffer.len() < config.network.resend_buffer_max_len {
                        ::log::info!("Adding response to resend queue, since sending it to {} failed with: {:#}", addr, err);

                        resend_buffer.push((response, canonical_addr));
                    } else {
                        ::log::warn!("Response resend buffer full, dropping response");
                    }
                }
                _ => {
                    ::log::warn!("Sending response to {} failed: {:#}", addr, err);
                }
            },
        }
    }
}

fn hex_slice(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 3);

    for chunk in bytes.chunks(4) {
        output.push_str(&hex::encode(chunk));
        output.push(' ');
    }
    
    output
}
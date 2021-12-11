use std::{collections::VecDeque, io::ErrorKind, marker::PhantomData, net::Shutdown, sync::Arc};

use aquatic_common::ValidUntil;
use aquatic_ws_protocol::{InMessage, OutMessage};
use mio::{net::TcpStream, Interest, Poll, Token};
use rustls::{ServerConfig, ServerConnection};
use tungstenite::{
    handshake::{server::NoCallback, MidHandshake},
    protocol::WebSocketConfig,
    HandshakeError, ServerHandshake,
};

use crate::common::ConnectionMeta;

const MAX_PENDING_MESSAGES: usize = 16;

type TlsStream = rustls::StreamOwned<ServerConnection, TcpStream>;

type WsHandshakeResult<S> =
    Result<tungstenite::WebSocket<S>, HandshakeError<ServerHandshake<S, NoCallback>>>;

type ConnectionReadResult<T> = ::std::io::Result<ConnectionReadStatus<T>>;

pub trait RegistryStatus {}

pub struct Registered;

impl RegistryStatus for Registered {}

pub struct NotRegistered;

impl RegistryStatus for NotRegistered {}

enum ConnectionReadStatus<T> {
    Message(T, InMessage),
    Ok(T),
    WouldBlock(T),
}

enum ConnectionState<R: RegistryStatus> {
    TlsHandshaking(TlsHandshaking<R>),
    WsHandshaking(WsHandshaking<R>),
    WsConnection(WsConnection<R>),
}

pub struct Connection<R: RegistryStatus> {
    pub valid_until: ValidUntil,
    meta: ConnectionMeta,
    state: ConnectionState<R>,
    pub message_queue: VecDeque<OutMessage>,
    pub interest: Interest,
    phantom_data: PhantomData<R>,
}

impl<R: RegistryStatus> Connection<R> {
    pub fn get_meta(&self) -> ConnectionMeta {
        self.meta
    }
}

impl Connection<NotRegistered> {
    pub fn new(
        tls_config: Arc<ServerConfig>,
        ws_config: WebSocketConfig,
        tcp_stream: TcpStream,
        valid_until: ValidUntil,
        meta: ConnectionMeta,
    ) -> Self {
        let state =
            ConnectionState::TlsHandshaking(TlsHandshaking::new(tls_config, ws_config, tcp_stream));

        Self {
            valid_until,
            meta,
            state,
            message_queue: Default::default(),
            interest: Interest::READABLE,
            phantom_data: PhantomData::default(),
        }
    }

    /// Read until stream blocks (or error occurs)
    ///
    /// Requires Connection not to be registered, since it might be dropped on errors
    pub fn read<F>(
        mut self,
        message_handler: &mut F,
    ) -> ::std::io::Result<Connection<NotRegistered>>
    where
        F: FnMut(ConnectionMeta, InMessage),
    {
        loop {
            let result = match self.state {
                ConnectionState::TlsHandshaking(inner) => inner.read(),
                ConnectionState::WsHandshaking(inner) => inner.read(),
                ConnectionState::WsConnection(inner) => inner.read(),
            };

            match result {
                Ok(ConnectionReadStatus::Message(state, message)) => {
                    self.state = state;

                    message_handler(self.meta, message);

                    // Stop looping even if WouldBlock wasn't necessarily reached. Otherwise,
                    // we might get stuck reading from this connection only. Since we register
                    // the connection again upon reinsertion into the ConnectionMap, we should
                    // be getting new events anyway.
                    return Ok(self);
                }
                Ok(ConnectionReadStatus::Ok(state)) => {
                    self.state = state;

                    ::log::debug!("read connection");
                }
                Ok(ConnectionReadStatus::WouldBlock(state)) => {
                    self.state = state;

                    ::log::debug!("reading connection would block");

                    return Ok(self);
                }
                Err(err) => {
                    ::log::debug!("Connection::read error: {}", err);

                    return Err(err);
                }
            }
        }
    }

    pub fn register(self, poll: &mut Poll, token: Token) -> Connection<Registered> {
        let state = match self.state {
            ConnectionState::TlsHandshaking(inner) => {
                ConnectionState::TlsHandshaking(inner.register(poll, token, self.interest))
            }
            ConnectionState::WsHandshaking(inner) => {
                ConnectionState::WsHandshaking(inner.register(poll, token, self.interest))
            }
            ConnectionState::WsConnection(inner) => {
                ConnectionState::WsConnection(inner.register(poll, token, self.interest))
            }
        };

        Connection {
            valid_until: self.valid_until,
            meta: self.meta,
            state,
            message_queue: self.message_queue,
            interest: self.interest,
            phantom_data: PhantomData::default(),
        }
    }

    pub fn close(self) {
        ::log::debug!("will close connection to {}", self.meta.naive_peer_addr);

        match self.state {
            ConnectionState::TlsHandshaking(inner) => inner.close(),
            ConnectionState::WsHandshaking(inner) => inner.close(),
            ConnectionState::WsConnection(inner) => inner.close(),
        }
    }
}

impl Connection<Registered> {
    pub fn write_or_queue_message(
        &mut self,
        poll: &mut Poll,
        message: OutMessage,
    ) -> ::std::io::Result<()> {
        let message_clone = message.clone();

        match self.write_message(message) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if self.message_queue.len() < MAX_PENDING_MESSAGES {
                    self.message_queue.push_back(message_clone);

                    if !self.interest.is_writable() {
                        self.interest = Interest::WRITABLE;

                        self.reregister(poll)?;
                    }
                } else {
                    ::log::info!("Connection::message_queue is full, dropping message");
                }

                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub fn write(&mut self, poll: &mut Poll) -> ::std::io::Result<()> {
        if let ConnectionState::WsConnection(_) = self.state {
            while let Some(message) = self.message_queue.pop_front() {
                let message_clone = message.clone();

                match self.write_message(message) {
                    Ok(()) => {}
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        // Can't make message queue longer than it was before pop_front
                        self.message_queue.push_front(message_clone);

                        return Ok(());
                    }
                    Err(err) => {
                        return Err(err);
                    }
                }
            }

            if self.message_queue.is_empty() {
                self.interest = Interest::READABLE;
            }

            self.reregister(poll)?;

            Ok(())
        } else {
            Err(std::io::Error::new(
                ErrorKind::NotConnected,
                "WebSocket connection not established",
            ))
        }
    }

    fn write_message(&mut self, message: OutMessage) -> ::std::io::Result<()> {
        if let ConnectionState::WsConnection(WsConnection {
            ref mut web_socket, ..
        }) = self.state
        {
            match web_socket.write_message(message.to_ws_message()) {
                Ok(_) => {}
                Err(tungstenite::Error::SendQueueFull(_message)) => {
                    return Err(std::io::Error::new(
                        ErrorKind::WouldBlock,
                        "Send queue full",
                    ))
                }
                Err(tungstenite::Error::Io(err)) => return Err(err),
                Err(err) => return Err(std::io::Error::new(ErrorKind::Other, err))?,
            }

            match web_socket.write_pending() {
                Ok(()) => Ok(()),
                Err(tungstenite::Error::Io(err)) => Err(err),
                Err(err) => Err(std::io::Error::new(ErrorKind::Other, err))?,
            }
        } else {
            Err(std::io::Error::new(
                ErrorKind::NotConnected,
                "WebSocket connection not established",
            ))
        }
    }

    pub fn reregister(&mut self, poll: &mut Poll) -> ::std::io::Result<()> {
        let token = Token(self.meta.connection_id.0);

        match self.state {
            ConnectionState::TlsHandshaking(ref mut inner) => {
                inner.reregister(poll, token, self.interest)
            }
            ConnectionState::WsHandshaking(ref mut inner) => {
                inner.reregister(poll, token, self.interest)
            }
            ConnectionState::WsConnection(ref mut inner) => {
                inner.reregister(poll, token, self.interest)
            }
        }
    }

    pub fn deregister(self, poll: &mut Poll) -> Connection<NotRegistered> {
        let state = match self.state {
            ConnectionState::TlsHandshaking(inner) => {
                ConnectionState::TlsHandshaking(inner.deregister(poll))
            }
            ConnectionState::WsHandshaking(inner) => {
                ConnectionState::WsHandshaking(inner.deregister(poll))
            }
            ConnectionState::WsConnection(inner) => {
                ConnectionState::WsConnection(inner.deregister(poll))
            }
        };

        Connection {
            valid_until: self.valid_until,
            meta: self.meta,
            state,
            message_queue: self.message_queue,
            interest: self.interest,
            phantom_data: PhantomData::default(),
        }
    }
}

struct TlsHandshaking<R: RegistryStatus> {
    tls_conn: ServerConnection,
    ws_config: WebSocketConfig,
    tcp_stream: TcpStream,
    phantom_data: PhantomData<R>,
}

impl TlsHandshaking<NotRegistered> {
    fn new(tls_config: Arc<ServerConfig>, ws_config: WebSocketConfig, stream: TcpStream) -> Self {
        Self {
            tls_conn: ServerConnection::new(tls_config).unwrap(),
            ws_config,
            tcp_stream: stream,
            phantom_data: PhantomData::default(),
        }
    }

    fn read(mut self) -> ConnectionReadResult<ConnectionState<NotRegistered>> {
        match self.tls_conn.read_tls(&mut self.tcp_stream) {
            Ok(0) => {
                return Err(::std::io::Error::new(
                    ErrorKind::ConnectionReset,
                    "Connection closed",
                ))
            }
            Ok(_) => match self.tls_conn.process_new_packets() {
                Ok(_) => {
                    while self.tls_conn.wants_write() {
                        self.tls_conn.write_tls(&mut self.tcp_stream)?;
                    }

                    if self.tls_conn.is_handshaking() {
                        Ok(ConnectionReadStatus::WouldBlock(
                            ConnectionState::TlsHandshaking(self),
                        ))
                    } else {
                        let tls_stream = TlsStream::new(self.tls_conn, self.tcp_stream);

                        WsHandshaking::handle_handshake_result(tungstenite::accept_with_config(
                            tls_stream,
                            Some(self.ws_config),
                        ))
                    }
                }
                Err(err) => {
                    let _ = self.tls_conn.write_tls(&mut self.tcp_stream);

                    Err(::std::io::Error::new(ErrorKind::InvalidData, err))
                }
            },
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                return Ok(ConnectionReadStatus::WouldBlock(
                    ConnectionState::TlsHandshaking(self),
                ))
            }
            Err(err) => return Err(err),
        }
    }

    fn register(
        mut self,
        poll: &mut Poll,
        token: Token,
        interest: Interest,
    ) -> TlsHandshaking<Registered> {
        poll.registry()
            .register(&mut self.tcp_stream, token, interest)
            .unwrap();

        TlsHandshaking {
            tls_conn: self.tls_conn,
            ws_config: self.ws_config,
            tcp_stream: self.tcp_stream,
            phantom_data: PhantomData::default(),
        }
    }

    fn close(self) {
        ::log::debug!("closing connection (TlsHandshaking state)");

        let _ = self.tcp_stream.shutdown(Shutdown::Both);
    }
}

impl TlsHandshaking<Registered> {
    fn deregister(mut self, poll: &mut Poll) -> TlsHandshaking<NotRegistered> {
        poll.registry().deregister(&mut self.tcp_stream).unwrap();

        TlsHandshaking {
            tls_conn: self.tls_conn,
            ws_config: self.ws_config,
            tcp_stream: self.tcp_stream,
            phantom_data: PhantomData::default(),
        }
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token: Token,
        interest: Interest,
    ) -> std::io::Result<()> {
        poll.registry()
            .reregister(&mut self.tcp_stream, token, interest)
    }
}

struct WsHandshaking<R: RegistryStatus> {
    mid_handshake: MidHandshake<ServerHandshake<TlsStream, NoCallback>>,
    phantom_data: PhantomData<R>,
}

impl WsHandshaking<NotRegistered> {
    fn read(self) -> ConnectionReadResult<ConnectionState<NotRegistered>> {
        Self::handle_handshake_result(self.mid_handshake.handshake())
    }

    fn handle_handshake_result(
        handshake_result: WsHandshakeResult<TlsStream>,
    ) -> ConnectionReadResult<ConnectionState<NotRegistered>> {
        match handshake_result {
            Ok(web_socket) => {
                let conn = ConnectionState::WsConnection(WsConnection {
                    web_socket,
                    phantom_data: PhantomData::default(),
                });

                Ok(ConnectionReadStatus::Ok(conn))
            }
            Err(HandshakeError::Interrupted(mid_handshake)) => {
                let conn = ConnectionState::WsHandshaking(WsHandshaking {
                    mid_handshake,
                    phantom_data: PhantomData::default(),
                });

                Ok(ConnectionReadStatus::WouldBlock(conn))
            }
            Err(HandshakeError::Failure(err)) => {
                return Err(std::io::Error::new(ErrorKind::InvalidData, err))
            }
        }
    }

    fn register(
        mut self,
        poll: &mut Poll,
        token: Token,
        interest: Interest,
    ) -> WsHandshaking<Registered> {
        let tcp_stream = &mut self.mid_handshake.get_mut().get_mut().sock;

        poll.registry()
            .register(tcp_stream, token, interest)
            .unwrap();

        WsHandshaking {
            mid_handshake: self.mid_handshake,
            phantom_data: PhantomData::default(),
        }
    }

    fn close(mut self) {
        ::log::debug!("closing connection (WsHandshaking state)");

        let tcp_stream = &mut self.mid_handshake.get_mut().get_mut().sock;

        let _ = tcp_stream.shutdown(Shutdown::Both);
    }
}

impl WsHandshaking<Registered> {
    fn deregister(mut self, poll: &mut Poll) -> WsHandshaking<NotRegistered> {
        let tcp_stream = &mut self.mid_handshake.get_mut().get_mut().sock;

        poll.registry().deregister(tcp_stream).unwrap();

        WsHandshaking {
            mid_handshake: self.mid_handshake,
            phantom_data: PhantomData::default(),
        }
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token: Token,
        interest: Interest,
    ) -> std::io::Result<()> {
        let tcp_stream = &mut self.mid_handshake.get_mut().get_mut().sock;

        poll.registry().reregister(tcp_stream, token, interest)
    }
}

struct WsConnection<R: RegistryStatus> {
    web_socket: tungstenite::WebSocket<TlsStream>,
    phantom_data: PhantomData<R>,
}

impl WsConnection<NotRegistered> {
    fn read(mut self) -> ConnectionReadResult<ConnectionState<NotRegistered>> {
        match self.web_socket.read_message() {
            Ok(
                message @ tungstenite::Message::Text(_) | message @ tungstenite::Message::Binary(_),
            ) => match InMessage::from_ws_message(message) {
                Ok(message) => {
                    ::log::debug!("received WebSocket message");

                    Ok(ConnectionReadStatus::Message(
                        ConnectionState::WsConnection(self),
                        message,
                    ))
                }
                Err(err) => Err(std::io::Error::new(ErrorKind::InvalidData, err)),
            },
            Ok(message) => {
                ::log::info!("received unexpected WebSocket message: {}", message);

                Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    "unexpected WebSocket message type",
                ))
            }
            Err(tungstenite::Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => {
                let conn = ConnectionState::WsConnection(self);

                Ok(ConnectionReadStatus::WouldBlock(conn))
            }
            Err(tungstenite::Error::Io(err)) => Err(err),
            Err(err) => Err(std::io::Error::new(ErrorKind::InvalidData, err)),
        }
    }

    fn register(
        mut self,
        poll: &mut Poll,
        token: Token,
        interest: Interest,
    ) -> WsConnection<Registered> {
        poll.registry()
            .register(self.web_socket.get_mut().get_mut(), token, interest)
            .unwrap();

        WsConnection {
            web_socket: self.web_socket,
            phantom_data: PhantomData::default(),
        }
    }

    fn close(mut self) {
        ::log::debug!("closing connection (WsConnection state)");

        let _ = self.web_socket.close(None);
        let _ = self.web_socket.write_pending();
    }
}

impl WsConnection<Registered> {
    fn deregister(mut self, poll: &mut Poll) -> WsConnection<NotRegistered> {
        poll.registry()
            .deregister(self.web_socket.get_mut().get_mut())
            .unwrap();

        WsConnection {
            web_socket: self.web_socket,
            phantom_data: PhantomData::default(),
        }
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token: Token,
        interest: Interest,
    ) -> std::io::Result<()> {
        poll.registry()
            .reregister(self.web_socket.get_mut().get_mut(), token, interest)
    }
}

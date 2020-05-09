use std::net::{SocketAddr};
use std::time::{Duration, Instant};
use std::io::ErrorKind;
use std::option::Option;

use slab::Slab;
use tungstenite::WebSocket;

use mio::{Events, Poll, Interest, Token};
use mio::net::{TcpListener, TcpStream};

use crate::common::*;
use crate::protocol::*;


pub struct PeerConnection {
    pub ws: WebSocket<TcpStream>,
    pub peer_socket_addr: SocketAddr,
    pub valid_until: ValidUntil,
}


pub fn run_socket_worker(
    address: SocketAddr,
    socket_worker_index: usize,
    in_message_sender: InMessageSender,
    out_message_receiver: OutMessageReceiver,
){
    let mut listener = TcpListener::bind(address).unwrap();
    let mut poll = Poll::new().expect("create poll");

    poll.registry()
        .register(&mut listener, Token(0), Interest::READABLE)
        .unwrap();

    let mut events = Events::with_capacity(1024); // FIXME: config

    let timeout = Duration::from_millis(50); // FIXME: config

    let mut connections: Slab<Option<PeerConnection>> = Slab::new();

    // Insert empty first entry to prevent assignment of index 0
    assert_eq!(connections.insert(None), 0);

    loop {
        poll.poll(&mut events, Some(timeout))
            .expect("failed polling");
        
        let valid_until = ValidUntil::new(600);

        for event in events.iter(){
            let token = event.token();

            if token.0 == 0 {
                accept_new_connections(
                    &mut listener,
                    &mut poll,
                    &mut connections,
                    valid_until
                );
            } else if event.is_readable(){
                read_and_forward_in_messages(
                    socket_worker_index,
                    &in_message_sender,
                    &mut poll,
                    &mut connections,
                    token,
                    valid_until
                );
            }
        }

        let now = Instant::now();

        // Close connections after some time of inactivity and write pending
        // messages (which is required after closing anyway.)
        //
        // FIXME: peers need to be removed too, wherever they are stored
        connections.retain(|_, opt_connection| {
            if let Some(connection) = opt_connection {
                if connection.valid_until.0 < now {
                    connection.ws.close(None).unwrap();
                }

                loop {
                    match connection.ws.write_pending(){
                        Err(tungstenite::Error::Io(err)) => {
                            if err.kind() == ErrorKind::WouldBlock {
                                break
                            }
                        },
                        Err(tungstenite::Error::ConnectionClosed) => {
                            // FIXME: necessary?
                            poll.registry()
                                .deregister(connection.ws.get_mut())
                                .unwrap();

                            return false;
                        },
                        _ => {}
                    }
                }
            }

            true
        });

        send_out_messages(
            out_message_receiver.drain(),
            &mut poll,
            &mut connections
        );
    }
}


fn accept_new_connections(
    listener: &mut TcpListener,
    poll: &mut Poll,
    connections: &mut Slab<Option<PeerConnection>>,
    valid_until: ValidUntil,
){
    loop {
        match listener.accept(){
            Ok((mut stream, src)) => {
                let entry = connections.vacant_entry();
                let token = Token(entry.key());

                poll.registry()
                    .register(&mut stream, token, Interest::READABLE)
                    .unwrap();
                
                // FIXME: will this cause issues due to blocking?
                // Should handshake be started manually below
                // instead?
                let ws = tungstenite::server::accept(stream).unwrap();

                let peer_connection = PeerConnection {
                    ws,
                    peer_socket_addr: src,
                    valid_until,
                };

                entry.insert(Some(peer_connection));
            },
            Err(err) => {
                if err.kind() == ErrorKind::WouldBlock {
                    break
                }

                eprint!("{}", err);
            }
        }
    }
}


pub fn read_and_forward_in_messages(
    socket_worker_index: usize,
    in_message_sender: &InMessageSender,
    poll: &mut Poll,
    connections: &mut Slab<Option<PeerConnection>>,
    poll_token: Token,
    valid_until: ValidUntil,
){
    loop {
        if let Some(Some(connection)) = connections.get_mut(poll_token.0){
            match connection.ws.read_message(){
                Ok(ws_message) => {
                    if let Some(in_message) = InMessage::from_ws_message(ws_message){
                        let meta = ConnectionMeta {
                            socket_worker_index,
                            socket_worker_slab_index: poll_token.0,
                            peer_socket_addr: connection.peer_socket_addr
                        };

                        in_message_sender.send((meta, in_message));
                    }

                    connection.valid_until = valid_until;
                },
                Err(tungstenite::Error::Io(err)) => {
                    if err.kind() == ErrorKind::WouldBlock {
                        break
                    }

                    eprint!("{}", err);
                },
                Err(tungstenite::Error::ConnectionClosed) => {
                    // FIXME: necessary?
                    poll.registry()
                        .deregister(connection.ws.get_mut())
                        .unwrap();

                    connections.remove(poll_token.0);
                },
                Err(err) => {
                    eprint!("{}", err);
                }
            }
        }
    }
}


pub fn send_out_messages(
    out_message_receiver: ::flume::Drain<(ConnectionMeta, OutMessage)>,
    poll: &mut Poll,
    connections: &mut Slab<Option<PeerConnection>>,
){
    // Read messages from channel, send to peers
    for (meta, out_message) in out_message_receiver {
        let opt_connection = connections
            .get_mut(meta.socket_worker_slab_index);

        if let Some(Some(connection)) = opt_connection {
            if connection.peer_socket_addr != meta.peer_socket_addr {
                eprintln!("socket worker: peer socket addrs didn't match");

                continue;
            }

            match connection.ws.write_message(out_message.to_ws_message()){
                Ok(()) => {},
                Err(tungstenite::Error::Io(err)) => {
                    if err.kind() == ErrorKind::WouldBlock {
                        continue;
                    }

                    eprint!("{}", err);
                },
                Err(tungstenite::Error::ConnectionClosed) => {
                    // FIXME: necessary?
                    poll.registry()
                        .deregister(connection.ws.get_mut())
                        .unwrap();

                    connections.remove(meta.socket_worker_slab_index);
                },
                Err(err) => {
                    eprint!("{}", err);
                },
            }
        }
    }
}
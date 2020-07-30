#![allow(unused, unused_variables)]

use std::{
    convert::{TryFrom, TryInto},
    fmt::Debug,
    result::Result as StdResult,
    sync::Arc,
};

use libp2p::{
    floodsub::{self, Floodsub, FloodsubEvent},
    identity,
    mdns::{Mdns, MdnsEvent},
    swarm::{NetworkBehaviourEventProcess, Swarm, SwarmEvent},
    Multiaddr, NetworkBehaviour, PeerId,
};

use async_trait::async_trait;
use conduit::{
    client_server::join_room_by_id_route, ConduitResult, Config, Database, Error as ConduitError,
    Ruma, State,
};
use http::{Method as HttpMethod, Request, Response};
use matrix_sdk::{
    api::r0::membership::join_room_by_id,
    identifiers::{DeviceId, RoomId, UserId},
    Client as MatrixClient, ClientConfig, Endpoint, Error as MatrixError, HttpClient,
    Result as MatrixResult, Session,
};
use reqwest::header::{HeaderValue, AUTHORIZATION};
use tokio::{
    runtime::Handle,
    sync::mpsc::{channel, Receiver, Sender},
    sync::RwLock,
};
use url::Url;

pub async fn start_p2p_server(
    spawn: Handle,
    client: P2PClient,
    user_id: Option<UserId>,
    device_id: Option<Box<DeviceId>>,
) -> StdResult<(), String> {
    // Create a random PeerId
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    println!("Local peer id: {:?}", local_peer_id);

    // Set up a an encrypted DNS-enabled TCP Transport over the Mplex and Yamux protocols
    let transport = libp2p::build_development_transport(local_key).map_err(|e| e.to_string())?;

    // Create a Floodsub topic
    let floodsub_topic = floodsub::Topic::new("chat");

    // Create a Swarm to manage peers and events
    let mut swarm = {
        let mdns = Mdns::new().map_err(|e| e.to_string())?;
        let mut behaviour = MyBehaviour {
            floodsub: Floodsub::new(local_peer_id.clone()),
            mdns,
            ignored_member: false,
        };

        behaviour.floodsub.subscribe(floodsub_topic);
        Swarm::new(transport, behaviour, local_peer_id)
    };

    // Reach out to another node if specified
    if let Some(to_dial) = std::env::args().nth(1) {
        let addr: Multiaddr = to_dial.parse().unwrap();
        Swarm::dial_addr(&mut swarm, addr).map_err(|e| e.to_string())?;
        println!("Dialed {:?}", to_dial)
    }

    // Read full lines from stdin

    // Listen on all interfaces and whatever port the OS assigns
    let listen_id = Swarm::listen_on(
        &mut swarm,
        "/ip4/0.0.0.0/tcp/0"
            .parse()
            .map_err(|e: libp2p::multiaddr::Error| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let database = Database::load_or_create(&Config::development()).map_err(|e| e.to_string())?;

    let mut listening = false;
    let mut is_authenticated = false;

    // This loop listens for events from incoming connections
    let recv: StdResult<(), String> = spawn
        .spawn(async move {
            loop {
                match swarm.next_event().await {
                    SwarmEvent::Behaviour(ev) => {
                        let meta = <join_room_by_id::Request as Endpoint>::METADATA.clone();
                        if meta.requires_authentication && !is_authenticated {
                            return Err(MatrixError::AuthenticationRequired.to_string());
                        }

                        // TODO make this a macro of some kind
                        match meta.path.split('/').collect::<Vec<_>>().as_slice() {
                            ["/_matrix", "client", "r0", "rooms", room_id, "join"] => {
                                let body = Ruma {
                                    json_body: serde_json::from_slice(ev.body()).ok(),
                                    body: join_room_by_id::Request::try_from(ev).unwrap(),
                                    user_id: user_id.clone(),
                                    device_id: device_id.clone(),
                                };
                                let response = conduit::client_server::join_room_by_id_route(
                                    State(&database),
                                    body,
                                    room_id.to_string(),
                                )
                                .map_err(|e| e.to_string())?;
                            }
                            ["/_matrix", "client", "r0", "rooms", room_id, "join"] => {}
                            ["/_matrix", "client", "r0", "rooms", room_id, "join"] => {}
                            // ["/_matrix", "client", "r0", "login"] if meta.method == http::Method::POST => {}
                            _ => unimplemented!(),
                        }
                    }
                    SwarmEvent::ConnectionEstablished {
                        peer_id,
                        endpoint,
                        num_established,
                    } => {}
                    SwarmEvent::ConnectionClosed {
                        peer_id,
                        endpoint,
                        num_established,
                        cause,
                    } => {}
                    SwarmEvent::IncomingConnection {
                        local_addr,
                        send_back_addr,
                    } => {}
                    SwarmEvent::IncomingConnectionError {
                        local_addr,
                        send_back_addr,
                        error,
                    } => {}
                    SwarmEvent::BannedPeer { peer_id, endpoint } => {}
                    SwarmEvent::UnreachableAddr {
                        peer_id,
                        address,
                        error,
                        attempts_remaining,
                    } => {}
                    SwarmEvent::UnknownPeerUnreachableAddr { address, error } => {}
                    SwarmEvent::NewListenAddr(addr) => {}
                    SwarmEvent::ExpiredListenAddr(addr) => {}
                    SwarmEvent::ListenerClosed { addresses, reason } => {}
                    SwarmEvent::ListenerError { error } => {}
                    SwarmEvent::Dialing(id) => {}
                }
            }
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub enum ClientInputEvents {
    Message(String),
}

pub struct P2PClient {
    from_user: Receiver<Request<Vec<u8>>>,
    p2p_client: MatrixClient,
}

impl P2PClient {
    pub async fn recv_input(&self) -> MatrixResult<Response<Vec<u8>>> {
        unimplemented!()
    }
}

#[async_trait]
impl HttpClient for P2PClient {
    /// The method abstracting sending request types and receiving response types.
    async fn send_request(
        &self,
        requires_auth: bool,
        homeserver: &Url,
        session: &Arc<RwLock<Option<Session>>>,
        method: HttpMethod,
        request: http::Request<Vec<u8>>,
    ) -> MatrixResult<reqwest::Response> {
        //
        Err(MatrixError::AuthenticationRequired)
    }
}

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "Request<Vec<u8>>")]
struct MyBehaviour {
    floodsub: Floodsub,
    mdns: Mdns,

    // Struct fields which do not implement NetworkBehaviour need to be ignored
    #[behaviour(ignore)]
    #[allow(dead_code)]
    ignored_member: bool,
}

impl NetworkBehaviourEventProcess<FloodsubEvent> for MyBehaviour {
    // Called when `floodsub` produces an event.
    fn inject_event(&mut self, message: FloodsubEvent) {
        if let FloodsubEvent::Message(message) = message {
            println!(
                "Received: '{:?}' from {:?}",
                String::from_utf8_lossy(&message.data),
                message.source
            );
        }
    }
}

impl NetworkBehaviourEventProcess<MdnsEvent> for MyBehaviour {
    // Called when `mdns` produces an event.
    fn inject_event(&mut self, event: MdnsEvent) {
        match event {
            MdnsEvent::Discovered(list) => {
                for (peer, _) in list {
                    self.floodsub.add_node_to_partial_view(peer);
                }
            }
            MdnsEvent::Expired(list) => {
                for (peer, _) in list {
                    if !self.mdns.has_node(&peer) {
                        self.floodsub.remove_node_from_partial_view(&peer);
                    }
                }
            }
        }
    }
}

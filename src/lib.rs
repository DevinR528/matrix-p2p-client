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
    Ruma, RumaResponse, State,
};
use http::{Method as HttpMethod, Request, Response};
use matrix_sdk::{
    api::r0::{membership::join_room_by_id, message::create_message_event},
    identifiers::{DeviceId, RoomId, UserId},
    Client as MatrixClient, ClientConfig, Endpoint, Error as MatrixError, HttpClient,
    Result as MatrixResult, Session,
};
use reqwest::header::{HeaderValue, AUTHORIZATION};
use tokio::{
    runtime::Handle,
    sync::mpsc::{channel, Receiver, Sender},
    sync::RwLock,
    task::{JoinError, JoinHandle},
};
use url::Url;

mod conduit_routes;
mod matrix_client;

pub use matrix_client::P2PClient;

// TODO make errors and an Error type

/// Returns a `JoinHandle` that controls the sending and receiving of request/response types
/// to and from the client sdk and conduit's route functions.
///
/// This future cannot be awaited as it is an infinite loop and would halt the program.
/// It must also be kept alive (do not `drop` this) until the program has ended.
pub async fn start_p2p_server(
    spawn: Handle,
    mut from_client: Receiver<http::Request<Vec<u8>>>,
    mut to_client: Sender<http::Response<Vec<u8>>>,
    connect_to: Option<String>,
    room_id: Option<String>,
    user_id: Option<UserId>,
    device_id: Option<Box<DeviceId>>,
) -> Result<JoinHandle<Result<(), String>>, String> {
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

        behaviour.floodsub.subscribe(floodsub_topic.clone());
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
    Ok(spawn.spawn(async move {
        loop {
            match swarm.next_event().await {
                SwarmEvent::Behaviour(ev) => {
                    // TODO check headers and stuff

                    let response = conduit_routes::process_request(
                        &database,
                        ev,
                        user_id.clone(),
                        device_id.clone(),
                        floodsub_topic.clone(),
                        is_authenticated,
                    )
                    .await?;

                    swarm
                        .floodsub
                        .publish(floodsub_topic.clone(), response.into_body());
                }
                SwarmEvent::NewListenAddr(_) => {}
                // TODO do we handle the low level connection stuff ??
                _ => {}
            }

            // TODO figure out a way to do this in a separate `spawn`
            // This processes the events received from the client and immediately responds
            match from_client.recv().await {
                Some(req) => {
                    let request_method = req.method().clone();

                    let response = conduit_routes::process_request(
                        &database,
                        req,
                        user_id.clone(),
                        device_id.clone(),
                        floodsub_topic.clone(),
                        is_authenticated,
                    )
                    .await?;

                    match request_method {
                        HttpMethod::GET => {
                            // The client has requested information from the "server".
                            if let Err(e) = to_client.send(response).await {
                                return Err("Failed to send response back to client".into());
                            }
                        }
                        // Everything else needs to be shared with the other peers.
                        HttpMethod::POST => {
                            swarm
                                .floodsub
                                .publish(floodsub_topic.clone(), response.into_body());
                        }
                        HttpMethod::PUT => {
                            swarm
                                .floodsub
                                .publish(floodsub_topic.clone(), response.into_body());
                        }
                        HttpMethod::DELETE => {
                            swarm
                                .floodsub
                                .publish(floodsub_topic.clone(), response.into_body());
                        }
                        _ => panic!("Unsupported method used {}", request_method),
                    }
                }
                None => {
                    if !listening {
                        for addr in Swarm::listeners(&swarm) {
                            println!("Listening on {:?}", addr);
                            listening = true;
                        }
                    }
                    // TODO do something productive
                    continue;
                }
            }
        }

        Ok(())
    }))
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

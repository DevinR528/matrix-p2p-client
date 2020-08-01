#![allow(unused, unused_variables)]

use std::{
    convert::{TryFrom, TryInto},
    fmt::Debug,
    ops::{Deref, DerefMut},
    result::Result as StdResult,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
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
#[allow(clippy::too_many_arguments)]
pub fn start_p2p_server(
    spawn: Handle,
    mut from_client: Receiver<http::Request<Vec<u8>>>,
    mut to_client: Sender<Result<http::Response<Vec<u8>>, String>>,
    db_path: String,
    connect_to: Option<String>,
    room_id: Option<String>,
    user_id: Option<UserId>,
    device_id: Option<Box<DeviceId>>,
) -> Result<(), String> {
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

    let mut config = Config::development();
    *config
        .extras
        .entry("database_path".to_owned())
        .or_insert_with(|| format!("./{}", db_path).into()) = format!("./{}", db_path).into();

    let database = Database::load_or_create(&config).map_err(|e| e.to_string())?;
    let database = Arc::new(database);
    let server_db = database.clone();

    let user_id_clone = user_id.clone();
    let device_id_clone = device_id.clone();

    let floodsub_clone = floodsub_topic.clone();
    let (mut to_swarm, mut swarm_receiver) = channel(1024);

    let mut listening = false;
    // This loop listens for events from incoming connections
    spawn.spawn(async move {
        loop {
            // This processes the events received from the client and immediately responds
            match from_client.try_recv() {
                Ok(req) => {
                    println!("SERVER LOOP");
                    let request_method = req.method().clone();

                    let response = conduit_routes::process_request(
                        &database,
                        req,
                        user_id_clone.clone(),
                        device_id_clone.clone(),
                        floodsub_clone.clone(),
                    );
                    // .await;

                    match request_method {
                        HttpMethod::GET => {
                            match response {
                                Err(e) => {
                                    // Send the error from the request back to the client
                                    if let Err(e) = to_client.send(Err(e)).await {
                                        panic!("GET failed to send response back to client");
                                    }
                                }
                                Ok(response) => {
                                    let body = response.body().clone();
                                    // The client has requested information from the "server".
                                    if let Err(e) = to_client.send(Ok(response)).await {
                                        panic!("GET failed to send response back to client");
                                    }
                                    // propagate the event out
                                    to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                            }
                        }
                        // Everything else needs to be shared with the other peers.
                        HttpMethod::POST => {
                            match response {
                                Err(e) => {
                                    // The client has requested information from the "server".
                                    if let Err(e) = to_client.send(Err(e)).await {
                                        panic!("GET failed to send response back to client");
                                    }
                                    // to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                                Ok(response) => {
                                    let body = response.body().clone();

                                    if let Err(e) = to_client.send(Ok(response)).await {
                                        panic!("GET failed to send response back to client");
                                    }

                                    to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                            }
                        }
                        HttpMethod::PUT => {
                            match response {
                                Err(e) => {
                                    // The client has requested information from the "server".
                                    if let Err(e) = to_client.send(Err(e)).await {
                                        panic!("GET failed to send response back to client");
                                    }
                                    // to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                                Ok(response) => {
                                    let body = response.body().clone();

                                    if let Err(e) = to_client.send(Ok(response)).await {
                                        panic!("GET failed to send response back to client");
                                    }

                                    to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                            }
                        }
                        HttpMethod::DELETE => {
                            match response {
                                Err(e) => {
                                    // The client has requested information from the "server".
                                    if let Err(e) = to_client.send(Err(e)).await {
                                        panic!("GET failed to send response back to client");
                                    }
                                    // to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                                Ok(response) => {
                                    let body = response.body().clone();

                                    if let Err(e) = to_client.send(Ok(response)).await {
                                        panic!("GET failed to send response back to client");
                                    }

                                    to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                            }
                        }
                        _ => panic!("Unsupported method used {}", request_method),
                    }
                }
                _ => tokio::task::yield_now().await,
            }
        }
    });

    spawn.spawn(async move {
        use futures::{future, prelude::*, StreamExt};
        use std::{
            ptr,
            task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
        };

        future::poll_fn(move |cx: &mut Context<'_>| {
            while let Ok((floodsub, resp)) = swarm_receiver.try_recv() {
                swarm.floodsub.publish(floodsub, resp);
            }

            loop {
                match swarm.poll_next_unpin(cx) {
                    Poll::Ready(Some(ev)) => {
                        // TODO check headers and stuff

                        // TODO this will have to be async
                        let response = conduit_routes::process_request(
                            &server_db,
                            ev,
                            user_id.clone(),
                            device_id.clone(),
                            floodsub_topic.clone(),
                        )?;

                        // let response = loop {
                        //     tokio::pin!(fut);
                        //     match (&mut fut).poll(cx) {
                        //         Poll::Ready(Ok(res)) => break res,
                        //         _ => continue,
                        //     }
                        // };

                        swarm
                            .floodsub
                            .publish(floodsub_topic.clone(), response.into_body());
                    }
                    Poll::Ready(None) => return Poll::Ready(Ok::<_, String>(())),
                    Poll::Pending => {
                        if !listening {
                            for addr in Swarm::listeners(&swarm) {
                                println!("Listening on {:?}", addr);
                                listening = true;
                            }
                        }
                        break;
                    }
                }
            }
            Poll::Pending
        })
        .await; // await `poll_fn` future that we made not the spawned task
    });

    Ok(())
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

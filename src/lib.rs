use std::sync::Arc;

use libp2p::{
    floodsub::{self, Floodsub, FloodsubEvent},
    identity,
    mdns::{Mdns, MdnsEvent},
    swarm::{NetworkBehaviourEventProcess, Swarm, SwarmEvent},
    Multiaddr, NetworkBehaviour, PeerId,
};

use conduit::{Config, Database};
use http::{Method as HttpMethod, Request};
use ruma::identifiers::{DeviceId, UserId};
use tokio::{
    runtime::Handle,
    sync::mpsc::{channel, Receiver, Sender},
};

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
    let _listen_id = Swarm::listen_on(
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
                    )
                    .await;

                    match request_method {
                        HttpMethod::GET => {
                            println!("GET");
                            match response {
                                Err(e) => {
                                    // Send the error from the request back to the client
                                    if let Err(e) = to_client.send(Err(e)).await {
                                        panic!("GET failed to send response back to client {}", e);
                                    }
                                }
                                Ok(response) => {
                                    let body = response.body().clone();
                                    // The client has requested information from the "server".
                                    if let Err(e) = to_client.send(Ok(response)).await {
                                        panic!("GET failed to send response back to client {}", e);
                                    }
                                    // propagate the event out
                                    // TODO we only want to send `Ok` responses right ?
                                    to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                            }
                        }
                        // Everything else needs to be shared with the other peers.
                        HttpMethod::POST => {
                            println!("POST");
                            match response {
                                Err(e) => {
                                    // The client has requested information from the "server".
                                    if let Err(e) = to_client.send(Err(e)).await {
                                        panic!("POST failed to send response back to client {}", e);
                                    }
                                }
                                Ok(response) => {
                                    let body = response.body().clone();

                                    if let Err(e) = to_client.send(Ok(response)).await {
                                        panic!("POST failed to send response back to client {}", e);
                                    }

                                    to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                            }
                        }
                        HttpMethod::PUT => {
                            println!("PUT");
                            match response {
                                Err(e) => {
                                    // The client has requested information from the "server".
                                    if let Err(e) = to_client.send(Err(e)).await {
                                        panic!("PUT failed to send response back to client {}", e);
                                    }
                                }
                                Ok(response) => {
                                    let body = response.body().clone();

                                    if let Err(e) = to_client.send(Ok(response)).await {
                                        panic!("PUT failed to send response back to client {}", e);
                                    }

                                    to_swarm.send((floodsub_clone.clone(), body)).await.unwrap();
                                }
                            }
                        }
                        HttpMethod::DELETE => {
                            println!("DELETE");
                            match response {
                                Err(e) => {
                                    // The client has requested information from the "server".
                                    if let Err(e) = to_client.send(Err(e)).await {
                                        panic!(
                                            "DELETE failed to send response back to client {}",
                                            e
                                        );
                                    }
                                }
                                Ok(response) => {
                                    let body = response.body().clone();

                                    if let Err(e) = to_client.send(Ok(response)).await {
                                        panic!(
                                            "DELETE failed to send response back to client {}",
                                            e
                                        );
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
        loop {
            // Connect ourselves
            if !listening {
                for addr in Swarm::listeners(&swarm) {
                    println!("Listening on {:?}", addr);
                    listening = true;
                }
            }

            // Race to handle the event that happens first then loop
            let res = tokio::select! {
                received = swarm_receiver.recv() => {
                    match received {
                        Some((floodsub, resp)) => {
                            swarm.floodsub.publish(floodsub, resp);
                            Ok::<_, String>(())
                        },
                        None => panic!("swarm_receiver's sender shutdown"),
                    }
                }
                event = swarm.next_event() => {
                    match event {
                        SwarmEvent::Behaviour(ev) => {
                            let response: Result<_, String> = conduit_routes::process_request(
                                &server_db,
                                ev,
                                user_id.clone(),
                                device_id.clone(),
                                floodsub_topic.clone(),
                            ).await;

                            // FIXME For some reason `?` does not work here
                            let response = match response {
                                Ok(res) => res,
                                Err(err) => return Err::<(), String>(err)
                            };

                            swarm
                                .floodsub
                                .publish(floodsub_topic.clone(), response.into_body());
                            Ok(())
                        }
                        _ => Ok(()),
                    }
                }
            };
            // Make sure no error happened during the race
            res?;
        }
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

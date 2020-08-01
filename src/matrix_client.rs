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

pub struct P2PClient {
    /// The other half of this is `from_client` in conduits loop.
    to_conduit: Arc<RwLock<Sender<Request<Vec<u8>>>>>,
    /// The other half of this is also in the second half of the conduit loop.
    from_conduit: Arc<RwLock<Receiver<Response<Vec<u8>>>>>,
    /// TODO do we need to know if the user is authed at this point ??
    is_authenticated: bool,
    /// TODO Not sure if this is needed either ??
    session: Arc<RwLock<Option<Session>>>,
    user_id: UserId,
    device_id: Box<DeviceId>,
}

impl P2PClient {
    pub fn new(
        user_id: UserId,
        device_id: Box<DeviceId>,
        to_conduit: Sender<Request<Vec<u8>>>,
        from_conduit: Receiver<Response<Vec<u8>>>,
    ) -> Self {
        Self {
            to_conduit: Arc::new(RwLock::new(to_conduit)),
            from_conduit: Arc::new(RwLock::new(from_conduit)),
            is_authenticated: false,
            session: Arc::new(RwLock::new(None)),
            user_id,
            device_id,
        }
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
        // if requires_auth && !self.is_authenticated {
        //     return Err(MatrixError::AuthenticationRequired);
        // }

        // Send a request to conduit directly
        if let Err(e) = self.to_conduit.write().await.send(request).await {
            return Err(MatrixError::MatrixError(matrix_sdk::BaseError::StateStore(
                e.to_string(),
            )));
        }

        // poll for the response
        // TODO I'm assuming that we cannot get responses out of order here
        // this may be wrong, check.
        loop {
            match self.from_conduit.write().await.recv().await {
                Some(resp) => {
                    return Ok(reqwest::Response::from(resp));
                }
                None => {
                    // TODO do something productive
                    continue;
                }
            }
        }
    }
}

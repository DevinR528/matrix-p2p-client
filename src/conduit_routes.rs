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
use matrix_sdk_common::Metadata;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use tokio::{
    runtime::Handle,
    sync::mpsc::{channel, Receiver, Sender},
    sync::RwLock,
};
use url::Url;

pub async fn process_request(
    database: &Database,
    ev: http::Request<Vec<u8>>,
    user_id: Option<UserId>,
    device_id: Option<Box<DeviceId>>,
    floodsub_topic: floodsub::Topic,
    is_authenticated: bool,
) -> StdResult<http::Response<std::vec::Vec<u8>>, String> {
    let path = ev.uri().to_string();

    // TODO make this a macro of some kind
    match path.split('/').collect::<Vec<_>>().as_slice() {
        ["/_matrix", "client", "r0", "rooms", room_id, "join"] if room_id.starts_with('!') => {
            let meta = <join_room_by_id::Request as Endpoint>::METADATA;
            if meta.requires_authentication && !is_authenticated {
                return Err(MatrixError::AuthenticationRequired.to_string());
            }

            let body = Ruma {
                json_body: serde_json::from_slice(ev.body()).ok(),
                body: join_room_by_id::Request::try_from(ev).unwrap(),
                user_id,
                device_id,
            };
            let response = conduit::client_server::join_room_by_id_route(
                State(&database),
                body,
                room_id.to_string(),
            )
            .map_err(|e| e.to_string())?;

            let RumaResponse(response) = response;
            http::Response::<Vec<u8>>::try_from(response).map_err(|e| e.to_string())
        }
        ["/_matrix", "client", "r0", "rooms", room_id, "send", event_type, txn_id]
            if room_id.starts_with('!') =>
        {
            let body = Ruma {
                json_body: serde_json::from_slice(ev.body()).ok(),
                body: create_message_event::Request::try_from(ev).unwrap(),
                user_id,
                device_id,
            };
            let response = conduit::client_server::create_message_event_route(
                State(&database),
                body,
                room_id.to_string(),
                event_type.to_string(),
                txn_id.to_string(),
            )
            .map_err(|e| e.to_string())?;

            let RumaResponse(response) = response;
            http::Response::<Vec<u8>>::try_from(response).map_err(|e| e.to_string())
        }
        // ["/_matrix", "client", "r0", "login"] if meta.method == http::Method::POST => {}
        _ => unimplemented!(),
    }
}

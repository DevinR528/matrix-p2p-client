use std::{convert::TryFrom, result::Result as StdResult};

use conduit::{Database, Error as ConduitError, Ruma, RumaResponse, State};
use libp2p::floodsub;
use ruma::{
    api::client::r0::{
        account::register::{self, RegistrationKind},
        membership::join_room_by_id,
        message::create_message_event,
        room::create_room,
        session::login,
        sync::sync_events,
        uiaa::{AuthData, UiaaInfo},
    },
    identifiers::{DeviceId, UserId},
};

pub async fn process_request(
    database: &Database,
    ev: http::Request<Vec<u8>>,
    user_id: Option<UserId>,
    device_id: Option<Box<DeviceId>>,
    _floodsub_topic: floodsub::Topic,
) -> StdResult<http::Response<std::vec::Vec<u8>>, String> {
    let path = ev.uri().to_string();

    // TODO make this a macro of some kind
    match path.split('/').collect::<Vec<_>>().as_slice() {
        ["", "_matrix", "client", "r0", "rooms", room_id, "join"] if room_id.starts_with("%21") => {
            println!("make join response conduit");
            let body = Ruma {
                json_body: serde_json::from_slice(ev.body()).ok(),
                body: join_room_by_id::Request::try_from(ev).unwrap(),
                sender_id: user_id,
                device_id,
            };
            let response = conduit::client_server::join_room_by_id_route(State(&database), body)
                .await
                .map_err(|e| e.to_string())?;

            println!("post join");

            let RumaResponse(response) = response;
            http::Response::<Vec<u8>>::try_from(response).map_err(|e| e.to_string())
        }
        ["", "_matrix", "client", "r0", "rooms", room_id, "send", _event_type, _txn_id]
            if room_id.starts_with("%21") =>
        {
            let body = Ruma {
                json_body: serde_json::from_slice(ev.body()).ok(),
                body: create_message_event::Request::try_from(ev).unwrap(),
                sender_id: user_id,
                device_id,
            };
            let response =
                conduit::client_server::create_message_event_route(State(&database), body)
                    .map_err(|e| e.to_string())?;

            let RumaResponse(response) = response;
            http::Response::<Vec<u8>>::try_from(response).map_err(|e| e.to_string())
        }
        ["", "_matrix", "client", "r0", "register?"] => {
            let body = ev.body().clone();
            let req = register::Request::try_from(ev).unwrap();

            let body = Ruma {
                json_body: serde_json::from_slice(body.as_slice()).ok(),
                body: req,
                sender_id: user_id.clone(),
                device_id: device_id.clone(),
            };

            let response = match conduit::client_server::register_route(State(&database), body) {
                Ok(res) => res,
                Err(ConduitError::Uiaa(UiaaInfo { flows, .. })) => {
                    if flows[0].stages[0] == "m.login.dummy" {
                        let reg = register::Request {
                            password: None,
                            username: None,
                            device_id: device_id.clone(),
                            initial_device_display_name: None,
                            auth: Some(AuthData::DirectRequest {
                                auth_parameters: std::collections::BTreeMap::new(),
                                session: None,
                                kind: "m.login.dummy".to_string(),
                            }),
                            kind: Some(RegistrationKind::User),
                            inhibit_login: false,
                        };

                        let body = Ruma {
                            json_body: serde_json::value::to_raw_value(&serde_json::json!({
                                "auth": AuthData::DirectRequest {
                                    auth_parameters: std::collections::BTreeMap::new(),
                                    kind: "m.login.dummy".to_string(),
                                    session: None,
                                },
                                "device_id": device_id
                            }))
                            .ok(),
                            body: reg,
                            sender_id: user_id,
                            device_id,
                        };
                        conduit::client_server::register_route(State(&database), body).unwrap()
                    } else {
                        panic!("conduit only uses 'm.login.dummy'")
                    }
                }
                _ => panic!("did not find UIAA error"),
            };
            let RumaResponse(response) = response;
            http::Response::<Vec<u8>>::try_from(response).map_err(|e| e.to_string())
        }
        ["", "_matrix", "client", "r0", "sync?"] => {
            let body = Ruma {
                json_body: serde_json::from_slice(ev.body()).ok(),
                body: sync_events::Request::try_from(ev).unwrap(),
                sender_id: user_id,
                device_id,
            };
            let response = conduit::client_server::sync_events_route(State(&database), body)
                .await
                .map_err(|e| e.to_string())?;

            let RumaResponse(response) = response;
            http::Response::<Vec<u8>>::try_from(response).map_err(|e| e.to_string())
        }
        ["", "_matrix", "client", "r0", "createRoom"] => {
            let body = Ruma {
                json_body: serde_json::from_slice(ev.body()).ok(),
                body: create_room::Request::try_from(ev).unwrap(),
                sender_id: user_id,
                device_id,
            };
            let response = conduit::client_server::create_room_route(State(&database), body)
                .map_err(|e| e.to_string())?;

            let RumaResponse(response) = response;
            http::Response::<Vec<u8>>::try_from(response).map_err(|e| e.to_string())
        }
        ["", "_matrix", "client", "r0", "login"] => {
            let body = Ruma {
                json_body: serde_json::from_slice(ev.body()).ok(),
                body: login::Request::try_from(ev).unwrap(),
                sender_id: user_id,
                device_id,
            };
            let response = conduit::client_server::login_route(State(&database), body)
                .map_err(|e| e.to_string())?;

            let RumaResponse(response) = response;
            http::Response::<Vec<u8>>::try_from(response).map_err(|e| e.to_string())
        }
        path => unimplemented!("{:?}", path),
    }
}

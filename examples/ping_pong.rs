use std::{convert::TryFrom, env, ops::Deref, sync::Arc, time::SystemTime};

use matrix_rust_p2p_client::{start_p2p_server, P2PClient};
use matrix_sdk::{
    api::r0::room::Visibility,
    events::{
        room::message::{MessageEventContent, RelatesTo, TextMessageEventContent},
        AnyPossiblyRedactedSyncMessageEvent, AnySyncMessageEvent, SyncMessageEvent,
    },
    identifiers::{RoomId, UserId},
    uuid::Uuid,
    Client as MatrixClient, ClientConfig, RegistrationBuilder, RoomBuilder, SyncSettings,
};
use tokio::{
    io::{self, AsyncBufReadExt},
    runtime::Runtime,
    sync::mpsc::channel,
};
use url::Url;

// static mut NUM: usize = 0;

// fn room_id() -> RoomId {
//     RoomId::try_from("!room:p2p.com").unwrap()
// }

// fn txn_id() -> String {
//     unsafe {
//         let x = NUM;
//         NUM += 1;
//         x
//     }
//     .to_string()
// }

fn main() {
    let args = env::args().collect::<Vec<_>>();

    let (connect_to, room_id) = match args.as_slice() {
        [] => (None, None),
        [_] => panic!("need a room_id to join"),
        [ip_addr, room_id, ..] => (Some(ip_addr.to_owned()), Some(room_id.to_owned())),
    };

    let mut runtime = Runtime::new().unwrap();

    let homeserver: Url = "https://p2p.com".parse().unwrap();
    let user_id = UserId::try_from("@test:p2p.com").unwrap();
    let device_id = "p2p_device".into();

    // send requests to conduit from the client
    let (to_conduit, from_client) = channel(1024);
    // send responses from conduit directly to the client
    let (to_client, from_conduit) = channel(1024);

    let p2p_client = P2PClient::new(to_conduit, from_conduit);
    let matrix =
        MatrixClient::new_with_client(homeserver, ClientConfig::default(), Arc::new(p2p_client))
            .unwrap();

    let mut stdin = io::BufReader::new(io::stdin()).lines();

    let server_reactor = runtime.handle().clone();
    let ui_reactor = runtime.handle().clone();

    runtime.block_on(async move {

        // start the database before making any requests to conduit
        let p2p_server = start_p2p_server(
            server_reactor,
            from_client,
            to_client,
            connect_to,
            room_id.clone(),
            Some(user_id),
            Some(device_id),
        );

        let room_id = login_and_join_room(&matrix, room_id).await.unwrap();
        println!("Room to join: {}", room_id.as_str());

        ui_reactor.spawn(async move {
            let mut timer = SystemTime::now();

            loop {
                match stdin.next_line().await {
                    Ok(Some(line)) => {
                        matrix
                            .room_send(
                                &room_id,
                                MessageEventContent::Text(TextMessageEventContent {
                                    body: line.to_string(),
                                    formatted: None,
                                    relates_to: None::<RelatesTo>,
                                }),
                                Some(Uuid::new_v4()),
                            )
                            .await
                            .unwrap();
                    }
                    Ok(None) => {
                        if timer.elapsed().unwrap().as_secs() > 10 {
                            timer = SystemTime::now();
                            matrix.sync(SyncSettings::default()).await.unwrap();
                            for message in matrix
                                .joined_rooms()
                                .read()
                                .await
                                .get(&room_id)
                                .unwrap()
                                .read()
                                .await
                                .messages
                                .iter()
                            {
                                println!(
                                    "Got message: {}",
                                    match message.deref() {
                                        AnyPossiblyRedactedSyncMessageEvent::Regular(
                                            AnySyncMessageEvent::RoomMessage(SyncMessageEvent{
                                                content: MessageEventContent::Text(TextMessageEventContent {
                                                    body, ..
                                                }),
                                                ..
                                            }),
                                        ) => body,
                                        _ => panic!("event redacted"),
                                    }
                                )
                            }
                        }
                    }
                    Err(_) => panic!("Stdin closed"),
                };
            }
        });

        p2p_server.await.unwrap();
    })
}

async fn login_and_join_room(
    client: &MatrixClient,
    room_id: Option<String>,
) -> Result<RoomId, String> {
    if let Some(id) = room_id {
        let room_id = RoomId::try_from(id).unwrap();
        client
            .join_room_by_id(&room_id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(room_id)
    } else {
        let mut registration = RegistrationBuilder::default();
        registration.username("user_1");
        registration.password("password");

        client
            .register_user(registration)
            .await
            .map_err(|e| e.to_string())?;

        let mut room = RoomBuilder::default();
        room.name("test");
        room.room_alias_name("test");
        room.visibility(Visibility::Public);

        let response = client.create_room(room).await.map_err(|e| e.to_string())?;

        Ok(response.room_id)
    }
}

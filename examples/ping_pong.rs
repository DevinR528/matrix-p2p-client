#![allow(clippy::eval_order_dependence)]

use std::{convert::TryFrom, env, ops::{Deref, DerefMut}, sync::Arc, time::SystemTime, sync::atomic::{AtomicBool, Ordering}};

use matrix_rust_p2p_client::{start_p2p_server, P2PClient};
use matrix_sdk::{
    api::r0::room::Visibility,
    events::{
        room::message::{MessageEventContent, RelatesTo, TextMessageEventContent},
        AnyPossiblyRedactedSyncMessageEvent, AnySyncMessageEvent, SyncMessageEvent,
    },
    identifiers::{RoomId, UserId, DeviceId},
    uuid::Uuid,
    Client as MatrixClient, ClientConfig, RegistrationBuilder, RoomBuilder, SyncSettings,
};
use tokio::{
    io::{self, AsyncBufReadExt},
    runtime::Runtime,
    sync::{mpsc::channel, RwLock},

};
use url::Url;

static mut NUM: usize = 0;

fn room_id() -> RoomId {
    RoomId::try_from("!room:p2p.com").unwrap()
}

fn txn_id() -> String {
    unsafe {
        let x = NUM;
        NUM += 1;
        x
    }
    .to_string()
}

fn main() {
    let args = env::args().collect::<Vec<_>>();

    let (connect_to, room_id_str) = match args.as_slice() {
        [] | [_, _] => panic!(),
        [_binary] => (None, None),
        [_bin, ip_addr, room_id, ..] => (Some(ip_addr.to_owned()), Some(room_id.to_owned())),
    };

    let mut runtime = Runtime::new().unwrap();

    let homeserver: Url = "https://localhost".parse().unwrap();
    let user_id = UserId::try_from("@test:localhost").unwrap();
    let device_id: Box<DeviceId> = "p2p_device".into();

    // send requests to conduit from the client
    let (to_conduit, from_client) = channel(1024);
    // send responses from conduit directly to the client
    let (to_client, from_conduit) = channel(1024);

    let p2p_client = P2PClient::new( user_id.clone(), device_id.clone(), to_conduit, from_conduit);
    let matrix =
        MatrixClient::new_with_client(homeserver, ClientConfig::default(), Arc::new(p2p_client))
            .unwrap();

    let mut stdin = io::BufReader::new(io::stdin()).lines();

    let server_reactor = runtime.handle().clone();
    let ui_reactor = runtime.handle().clone();

    runtime.block_on(async move {

        let flag = Arc::new(AtomicBool::default());
        let sync_ready = Arc::new(AtomicBool::default());

        // start the database before making any requests to conduit
        let _p2p_server = start_p2p_server(
            server_reactor,
            from_client,
            to_client,
            room_id_str.clone().unwrap_or("user_one".to_owned()),
            connect_to,
            room_id_str.clone(),
            Some(user_id),
            Some(device_id),
        ).unwrap();

        
        let room_id = Arc::new(RwLock::new(RoomId::try_from("!fake:room").unwrap()));
        let room_id_copy = room_id.clone();
        let room_id_copy2 = room_id.clone();

        let matrix_copy = matrix.clone();

        let flag_clone = flag.clone();
        let flag_copy2 = flag.clone();
        let sync_ready_copy = sync_ready.clone();

        let login = ui_reactor.spawn(async move {
                let mut rid = room_id.write().await;
                let rid = rid.deref_mut();
                *rid = login_and_join_room(&matrix_copy, room_id_str, flag_copy2, sync_ready_copy).await.unwrap();
        });

        ui_reactor.spawn(async move {
            let room_id = Arc::clone(&room_id_copy);
            let mut timer = SystemTime::now();

            loop {
                if !flag_clone.load(Ordering::SeqCst) {
                    std::sync::atomic::spin_loop_hint();
                    continue;
                }
                match stdin.next_line().await {
                    Ok(Some(line)) => {
                        println!("sending");
                        matrix
                            .room_send(
                                room_id.read().await.deref(),
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
                    Ok(None) if sync_ready.load(Ordering::SeqCst) => {
                        if timer.elapsed().unwrap().as_secs() > 10 {
                            println!("syncing");
                            timer = SystemTime::now();
                            matrix.sync(SyncSettings::default()).await.unwrap();
                            for message in matrix
                                .joined_rooms()
                                .read()
                                .await
                                .get(room_id.read().await.deref())
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
                    Ok(None) => continue,
                    Err(_) => panic!("Stdin closed"),
                };
            }
        });

        login.await.unwrap();
        println!("Room to join: {}", room_id_copy2.read().await.as_str());
    })
}

async fn login_and_join_room(
    client: &MatrixClient,
    room_id: Option<String>,
    flag: Arc<AtomicBool>,
    sync_ready: Arc<AtomicBool>,
) -> Result<RoomId, String> {
    if let Some(id) = room_id {
        let room_id = RoomId::try_from(id).unwrap();

        // start the UI loop but don't call anything
        flag.swap(true, Ordering::SeqCst);

        let mut registration = RegistrationBuilder::default();
        registration.username("user_2");
        registration.password("password");

        client
            .register_user(registration)
            .await
            .map_err(|e| e.to_string())?;

        // now we can sync since we are registered
        sync_ready.swap(true, Ordering::SeqCst);

        client
            .join_room_by_id(&room_id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(room_id)
    } else {
        // start the UI loop but don't call anything
        flag.swap(true, Ordering::SeqCst);

        let mut registration = RegistrationBuilder::default();
        registration.username("test")
            .password("password")
            .device_id("p2p_device");

        client
            .register_user(registration)
            .await
            .map_err(|e| e.to_string())?;


        // now we can sync since we are registered
        sync_ready.swap(true, Ordering::SeqCst);

        let mut room = RoomBuilder::default();
        room.name("test");
        room.room_alias_name("test");
        room.visibility(Visibility::Public);

        let response = client.create_room(room).await.map_err(|e| e.to_string())?;

        Ok(response.room_id)
    }
}

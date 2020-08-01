#![allow(clippy::eval_order_dependence)]

use std::{
    convert::TryFrom,
    env,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    time::SystemTime,
};

use matrix_rust_p2p_client::{start_p2p_server, P2PClient};
use matrix_sdk::{
    api::r0::room::Visibility,
    events::{
        room::message::{MessageEventContent, RelatesTo, TextMessageEventContent},
        AnyPossiblyRedactedSyncMessageEvent, AnySyncMessageEvent, SyncMessageEvent,
    },
    identifiers::{DeviceId, RoomId, UserId},
    uuid::Uuid,
    BaseError as MxBaseError, Client as MatrixClient, ClientConfig, Error as MxError,
    RegistrationBuilder, RoomBuilder, Session, SyncSettings,
};
use tokio::{
    io::{self, AsyncBufReadExt},
    runtime::Runtime,
    sync::{mpsc::channel, RwLock},
};
use url::Url;

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

    let p2p_client = P2PClient::new(user_id.clone(), device_id.clone(), to_conduit, from_conduit);
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
            room_id_str.clone().unwrap_or_else(|| "user_one".to_owned()),
            connect_to,
            room_id_str.clone(),
            Some(user_id),
            Some(device_id),
        )
        .unwrap();

        let room_id = Arc::new(RwLock::new(RoomId::try_from("!fake:room").unwrap()));
        let room_id_copy = room_id.clone();

        let matrix_copy = matrix.clone();

        let flag_clone = flag.clone();
        let sync_ready_copy = sync_ready.clone();

        let login = ui_reactor.spawn(async move {
            let mut rid = room_id.write().await;
            let rid = rid.deref_mut();
            *rid = login_and_join_room(&matrix_copy, room_id_str, flag, sync_ready_copy)
                .await
                .unwrap();
        });

        let ui = ui_reactor.spawn(async move {
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

                        // Lets see if we can get some messages
                        sync_with_conduit(&mut timer, &matrix, room_id.read().await.deref()).await;
                    }
                    Ok(None) if sync_ready.load(Ordering::SeqCst) => {
                        sync_with_conduit(&mut timer, &matrix, room_id.read().await.deref()).await;
                    }
                    Ok(None) => {
                        tokio::task::yield_now().await;
                    }
                    Err(_) => panic!("Stdin closed"),
                };
            }
        });

        login.await.unwrap();
        ui.await.unwrap();
    })
}

async fn sync_with_conduit(timer: &mut SystemTime, matrix: &MatrixClient, room_id: &RoomId) {
    if timer.elapsed().unwrap().as_secs() > 3 {
        println!("syncing");
        *timer = SystemTime::now();
        matrix.sync(SyncSettings::default()).await.unwrap();
        for message in matrix
            .joined_rooms()
            .read()
            .await
            .get(room_id)
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
                        AnySyncMessageEvent::RoomMessage(SyncMessageEvent {
                            content: MessageEventContent::Text(TextMessageEventContent { body, .. }),
                            ..
                        }),
                    ) => body,
                    _ => panic!("event redacted"),
                }
            )
        }
    }
}

async fn login_and_join_room(
    client: &MatrixClient,
    room_name: Option<String>,
    flag: Arc<AtomicBool>,
    sync_ready: Arc<AtomicBool>,
) -> Result<RoomId, String> {
    if let Some(id) = room_name {
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
        registration
            .username("devin")
            .password("devin_password")
            .device_id("p2p_device");

        let reg = client
            .register_user(registration)
            .await
            .map_err(|e| e.to_string())?;

        client
            .restore_login(Session {
                access_token: reg.access_token.clone().unwrap(),
                device_id: reg.device_id.clone().unwrap(),
                user_id: reg.user_id.clone(),
            })
            .await
            .unwrap();

        // now we can sync since we are registered
        sync_ready.swap(true, Ordering::SeqCst);

        let mut room = RoomBuilder::default();
        room.name("test");
        room.room_alias_name("test");
        room.visibility(Visibility::Public);

        println!("create ROOM");

        let room_id = match client.create_room(room).await {
            Ok(res) => res.room_id,
            Err(err) => match err {
                MxError::MatrixError(MxBaseError::StateStore(err)) => {
                    if err.starts_with("M_ROOM_IN_USE") {
                        client
                            .join_room_by_id(
                                &RoomId::try_from("!B0v1yXN99lkXRsU6TX:localhost").unwrap(),
                            )
                            .await
                            .unwrap()
                            .room_id
                    } else {
                        panic!("{}", err)
                    }
                }
                e => panic!("{}", e),
            },
        };

        println!("Room to join: {}", room_id.as_str());

        Ok(room_id)
    }
}

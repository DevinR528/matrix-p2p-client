use std::sync::Arc;

use async_trait::async_trait;
use http::{Method as HttpMethod, Request, Response};
use matrix_sdk::{Error as MatrixError, HttpClient, Result as MatrixResult, Session};
use ruma::identifiers::{DeviceId, UserId};
use tokio::sync::{
    mpsc::{Receiver, Sender},
    RwLock,
};
use url::Url;

pub type ConduitSender = Arc<RwLock<Receiver<Result<Response<Vec<u8>>, String>>>>;
pub type ConduitReceiver = Arc<RwLock<Sender<Request<Vec<u8>>>>>;

#[allow(unused)]
pub struct P2PClient {
    /// The other half of this is `from_client` in conduits loop.
    to_conduit: ConduitReceiver,
    /// The other half of this is also in the second half of the conduit loop.
    from_conduit: ConduitSender,
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
        from_conduit: Receiver<Result<Response<Vec<u8>>, String>>,
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
        _requires_auth: bool,
        _homeserver: &Url,
        _session: &Arc<RwLock<Option<Session>>>,
        _method: HttpMethod,
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
        match self.from_conduit.write().await.recv().await {
            Some(Ok(resp)) => Ok(reqwest::Response::from(resp)),
            Some(Err(err)) => Err(MatrixError::MatrixError(matrix_sdk::BaseError::StateStore(
                err,
            ))),
            _ => panic!("from_conduit channel shutdown"),
        }
    }
}

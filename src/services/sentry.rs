use crate::{
    eth::*,
    grpc::sentry::{
        sentry_server::*, InboundMessage, OutboundMessageData, PeerMinBlockRequest, SentPeers,
    },
    CapabilityServerImpl,
};
use async_trait::async_trait;
use devp2p::*;
use futures::{stream::FuturesUnordered, Stream};
use num_traits::ToPrimitive;
use std::{
    convert::{identity, TryFrom},
    pin::Pin,
    sync::Arc,
};
use tokio::sync::broadcast::Sender as BroadcastSender;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use tonic::Response;

pub type InboundMessageStream =
    Pin<Box<dyn Stream<Item = anyhow::Result<InboundMessage, tonic::Status>> + Send + Sync>>;

pub struct SentryService {
    capability_server: Arc<CapabilityServerImpl>,
}

impl SentryService {
    pub fn new(capability_server: Arc<CapabilityServerImpl>) -> Self {
        Self { capability_server }
    }
}

impl SentryService {
    async fn send_by_predicate<F, IT>(
        &self,
        request: Option<OutboundMessageData>,
        pred: F,
    ) -> SentPeers
    where
        F: FnOnce(&CapabilityServerImpl) -> IT,
        IT: IntoIterator<Item = PeerId>,
    {
        if let Some(request) = request {
            let data = request.data;
            let id = request.id.to_usize().unwrap();

            return SentPeers {
                peers: (pred)(&*self.capability_server)
                    .into_iter()
                    .map(|peer| {
                        let data = data.clone();
                        async move {
                            if let Some(sender) = self.capability_server.sender(peer) {
                                if sender
                                    .send(OutboundEvent::Message {
                                        capability_name: capability_name(),
                                        message: Message { id, data },
                                    })
                                    .await
                                    .is_ok()
                                {
                                    return Some(peer);
                                }
                            }

                            None
                        }
                    })
                    .collect::<FuturesUnordered<_>>()
                    .filter_map(identity)
                    .map(|peer_id| peer_id.into())
                    .collect::<Vec<_>>()
                    .await,
            };
        }

        SentPeers { peers: vec![] }
    }

    fn make_channel(
        &self,
        f: impl Fn(&CapabilityServerImpl) -> &BroadcastSender<InboundMessage>,
    ) -> Response<InboundMessageStream> {
        Response::new(Box::pin(
            BroadcastStream::new((f)(&self.capability_server).subscribe())
                .filter_map(|res| res.ok().map(Ok)),
        ))
    }
}

#[async_trait]
impl Sentry for SentryService {
    async fn penalize_peer(
        &self,
        request: tonic::Request<crate::grpc::sentry::PenalizePeerRequest>,
    ) -> Result<Response<()>, tonic::Status> {
        let peer = request
            .into_inner()
            .peer_id
            .ok_or_else(|| tonic::Status::invalid_argument("no peer id"))?
            .into();
        if let Some(sender) = self.capability_server.sender(peer) {
            let _ = sender
                .send(OutboundEvent::Disconnect {
                    reason: DisconnectReason::DisconnectRequested,
                })
                .await;
        }

        Ok(Response::new(()))
    }

    async fn send_message_by_min_block(
        &self,
        request: tonic::Request<crate::grpc::sentry::SendMessageByMinBlockRequest>,
    ) -> Result<Response<SentPeers>, tonic::Status> {
        let crate::grpc::sentry::SendMessageByMinBlockRequest { data, min_block } =
            request.into_inner();
        Ok(Response::new(
            self.send_by_predicate(data, |capability_server| {
                capability_server
                    .block_tracker
                    .read()
                    .peers_with_min_block(min_block)
            })
            .await,
        ))
    }

    async fn send_message_by_id(
        &self,
        request: tonic::Request<crate::grpc::sentry::SendMessageByIdRequest>,
    ) -> Result<Response<SentPeers>, tonic::Status> {
        let crate::grpc::sentry::SendMessageByIdRequest { peer_id, data } = request.into_inner();

        let peer = peer_id
            .ok_or_else(|| tonic::Status::invalid_argument("no peer id"))?
            .into();

        Ok(Response::new(
            self.send_by_predicate(data, |_| std::iter::once(peer))
                .await,
        ))
    }

    async fn send_message_to_random_peers(
        &self,
        request: tonic::Request<crate::grpc::sentry::SendMessageToRandomPeersRequest>,
    ) -> Result<Response<SentPeers>, tonic::Status> {
        let crate::grpc::sentry::SendMessageToRandomPeersRequest { max_peers, data } =
            request.into_inner();

        Ok(Response::new(
            self.send_by_predicate(data, |capability_server| {
                capability_server
                    .all_peers()
                    .into_iter()
                    .take(max_peers as usize)
            })
            .await,
        ))
    }

    async fn send_message_to_all(
        &self,
        request: tonic::Request<OutboundMessageData>,
    ) -> Result<Response<SentPeers>, tonic::Status> {
        Ok(Response::new(
            self.send_by_predicate(Some(request.into_inner()), |capability_server| {
                capability_server.all_peers()
            })
            .await,
        ))
    }

    async fn peer_min_block(
        &self,
        request: tonic::Request<PeerMinBlockRequest>,
    ) -> Result<Response<()>, tonic::Status> {
        let PeerMinBlockRequest { peer_id, min_block } = request.into_inner();

        let peer = peer_id
            .ok_or_else(|| tonic::Status::invalid_argument("no peer id"))?
            .into();

        self.capability_server
            .block_tracker
            .write()
            .set_block_number(peer, min_block, false);

        Ok(Response::new(()))
    }

    async fn set_status(
        &self,
        request: tonic::Request<crate::grpc::sentry::StatusData>,
    ) -> Result<Response<()>, tonic::Status> {
        let s = FullStatusData::try_from(request.into_inner())
            .map_err(|e| tonic::Status::invalid_argument(e.to_string()))?;

        *self.capability_server.status_message.write() = Some(s);

        Ok(Response::new(()))
    }

    type ReceiveMessagesStream = InboundMessageStream;

    async fn receive_messages(
        &self,
        _request: tonic::Request<()>,
    ) -> Result<Response<Self::ReceiveMessagesStream>, tonic::Status> {
        Ok(self.make_channel(|c| &c.data_sender))
    }

    type ReceiveUploadMessagesStream = InboundMessageStream;

    async fn receive_upload_messages(
        &self,
        _request: tonic::Request<()>,
    ) -> Result<Response<Self::ReceiveUploadMessagesStream>, tonic::Status> {
        Ok(self.make_channel(|c| &c.upload_requests_sender))
    }

    type ReceiveTxMessagesStream = InboundMessageStream;

    async fn receive_tx_messages(
        &self,
        _request: tonic::Request<()>,
    ) -> Result<Response<Self::ReceiveTxMessagesStream>, tonic::Status> {
        Ok(self.make_channel(|c| &c.tx_message_sender))
    }
}

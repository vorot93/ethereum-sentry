use super::{Control, DataProvider, Txpool};
use crate::{
    eth::*,
    grpc::sentry::{sentry_server::*, OutboundMessageData, SentPeers},
    CapabilityServerImpl,
};
use async_trait::async_trait;
use bytes::Bytes;
use devp2p::*;
use futures::stream::FuturesUnordered;
use num_traits::ToPrimitive;
use std::{convert::identity, fmt::Debug, sync::Arc};
use tokio_stream::StreamExt;
use tonic::Response;

#[derive(Clone, Debug)]
pub struct SentryService<C, DP, TxPool>
where
    C: Debug,
    DP: Debug,
    TxPool: Debug,
{
    capability_server: Arc<CapabilityServerImpl<C, DP, TxPool>>,
}

impl<C, DP, TxPool> SentryService<C, DP, TxPool>
where
    C: Debug,
    DP: Debug,
    TxPool: Debug,
{
    pub fn new(capability_server: Arc<CapabilityServerImpl<C, DP, TxPool>>) -> Self {
        Self { capability_server }
    }
}

impl<C, DP, TxPool> SentryService<C, DP, TxPool>
where
    C: Control,
    DP: DataProvider,
    TxPool: Txpool,
{
    async fn send_by_predicate<F, IT>(
        &self,
        request: Option<OutboundMessageData>,
        pred: F,
    ) -> SentPeers
    where
        F: FnOnce(&CapabilityServerImpl<C, DP, TxPool>) -> IT,
        IT: IntoIterator<Item = PeerId>,
    {
        if let Some(data) = request {
            if let Some(id) = MessageId::from_outbound_message_id(data.id) {
                let data = Bytes::from(data.data);
                let id = id.to_usize().unwrap();

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
                        .map(|peer_id| peer_id.to_fixed_bytes().to_vec())
                        .collect::<Vec<_>>()
                        .await,
                };
            }
        }

        SentPeers { peers: vec![] }
    }
}

#[async_trait]
impl<C, DP, TxPool> Sentry for SentryService<C, DP, TxPool>
where
    C: Control,
    DP: DataProvider,
    TxPool: Txpool,
{
    async fn penalize_peer(
        &self,
        request: tonic::Request<crate::grpc::sentry::PenalizePeerRequest>,
    ) -> Result<Response<()>, tonic::Status> {
        let peer = hex::encode(&request.into_inner().peer_id)
            .parse::<PeerId>()
            .map_err(|e| tonic::Status::invalid_argument(e.to_string()))?;
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

        let peer = hex::encode(&peer_id)
            .parse::<PeerId>()
            .map_err(|e| tonic::Status::invalid_argument(e.to_string()))?;

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
}

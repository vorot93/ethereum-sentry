use crate::{
    eth::{Forks, StatusData},
    grpc::control::{control_client::ControlClient, *},
};
use anyhow::anyhow;
use async_trait::async_trait;
use auto_impl::auto_impl;
use std::fmt::Debug;
use tonic::transport::Channel;
use tokio_compat_02::FutureExt;

#[async_trait]
#[auto_impl(&, Box, Arc)]
pub trait Control: Debug + Send + Sync + 'static {
    async fn forward_inbound_message(&self, message: InboundMessage) -> anyhow::Result<()>;
    async fn get_status_data(&self) -> anyhow::Result<StatusData>;
}

#[derive(Debug)]
pub struct GrpcControl {
    client: ControlClient<Channel>,
}

impl GrpcControl {
    pub async fn connect(addr: String) -> anyhow::Result<Self> {
        Ok(Self {
            client: ControlClient::connect(addr).compat().await?,
        })
    }
}

#[async_trait]
impl Control for GrpcControl {
    async fn forward_inbound_message(&self, message: InboundMessage) -> anyhow::Result<()> {
        self.client.clone().forward_inbound_message(message).await?;

        Ok(())
    }

    async fn get_status_data(&self) -> anyhow::Result<StatusData> {
        let status_data = self.client.clone().get_status(()).await?.into_inner();

        let fork_data = status_data
            .fork_data
            .ok_or_else(|| anyhow!("no fork data"))?;

        Ok(StatusData {
            network_id: status_data.network_id,
            total_difficulty: hex::encode(status_data.total_difficulty).parse()?,
            best_hash: hex::encode(status_data.best_hash).parse()?,
            fork_data: Forks {
                genesis: hex::encode(fork_data.genesis).parse()?,
                forks: fork_data.forks.into_iter().collect(),
            },
        })
    }
}

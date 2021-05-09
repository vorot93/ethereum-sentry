use crate::types::*;
use async_stream::stream;
use futures::{stream::BoxStream, StreamExt};
use std::{collections::HashMap, net::SocketAddr, pin::Pin, task::Poll, time::Duration};
use tokio::time::sleep;
use tokio_stream::Stream;

#[cfg(feature = "discv4")]
mod v4;

#[cfg(feature = "discv4")]
pub use self::v4::{Discv4, Discv4Builder};
#[cfg(feature = "discv4")]
pub use discv4;

#[cfg(feature = "discv5")]
mod v5;

#[cfg(feature = "discv5")]
pub use self::v5::Discv5;
#[cfg(feature = "discv5")]
pub use discv5;

#[cfg(feature = "dnsdisc")]
mod dns;

#[cfg(feature = "dnsdisc")]
pub use self::dns::DnsDiscovery;
#[cfg(feature = "dnsdisc")]
pub use dnsdisc;

pub type Discovery = BoxStream<'static, anyhow::Result<NodeRecord>>;

pub struct StaticNodes(Pin<Box<dyn Stream<Item = anyhow::Result<NodeRecord>> + Send + 'static>>);

impl StaticNodes {
    pub fn new(nodes: HashMap<SocketAddr, PeerId>, delay: Duration) -> Self {
        Self(Box::pin(stream! {
            loop {
                for (&addr, &id) in &nodes {
                    yield Ok(NodeRecord { id, addr });
                    sleep(delay).await;
                }
            }
        }))
    }
}

impl Stream for StaticNodes {
    type Item = anyhow::Result<NodeRecord>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.0.poll_next_unpin(cx)
    }
}

mod dummy;
mod grpc;

pub use self::{dummy::*, grpc::*};
use crate::grpc::txpool::ImportResult;
use async_trait::async_trait;
use auto_impl::auto_impl;
use ethereum_types::*;
use std::fmt::Debug;

#[async_trait]
#[auto_impl(&, Box, Arc)]
pub trait Txpool: Debug + Send + Sync + 'static {
    async fn find_unknown_transactions(&self, txs: &[H256]) -> anyhow::Result<Vec<H256>>;
    async fn import_transactions(&self, txs: Vec<Vec<u8>>) -> anyhow::Result<Vec<ImportResult>>;
    async fn get_transactions(&self, txs: &[H256]) -> anyhow::Result<Vec<Vec<u8>>>;
}

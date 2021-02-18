use super::Txpool;
use crate::grpc::txpool::ImportResult;
use async_trait::async_trait;
use ethereum_types::H256;

#[derive(Debug)]
pub struct DummyTxpool;

#[async_trait]
impl Txpool for DummyTxpool {
    async fn find_unknown_transactions(&self, _: &[H256]) -> anyhow::Result<Vec<H256>> {
        Ok(vec![])
    }

    async fn import_transactions(&self, txs: Vec<Vec<u8>>) -> anyhow::Result<Vec<ImportResult>> {
        Ok(vec![ImportResult::AlreadyExists; txs.len()])
    }

    async fn get_transactions(&self, _: &[H256]) -> anyhow::Result<Vec<Vec<u8>>> {
        Ok(vec![])
    }
}

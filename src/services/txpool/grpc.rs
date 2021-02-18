use super::Txpool;
use crate::grpc::txpool::{
    txpool_client::TxpoolClient, GetTransactionsRequest, ImportRequest, ImportResult, TxHashes,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use ethereum_types::H256;
use tonic::transport::Channel;

#[derive(Debug)]
pub struct GrpcTxpool {
    client: TxpoolClient<Channel>,
}

impl GrpcTxpool {
    pub async fn connect(dst: String) -> anyhow::Result<Self> {
        Ok(GrpcTxpool {
            client: TxpoolClient::connect(dst).await?,
        })
    }
}

#[async_trait]
impl Txpool for GrpcTxpool {
    async fn find_unknown_transactions(&self, txs: &[H256]) -> Result<Vec<H256>> {
        let hashes = txs.iter().map(|h| h.as_bytes().to_vec()).collect();
        self.client
            .clone()
            .find_unknown_transactions(TxHashes { hashes })
            .await?
            .into_inner()
            .hashes
            .into_iter()
            .map(|h| {
                if h.len() == H256::len_bytes() {
                    Ok(H256::from_slice(&h))
                } else {
                    Err(anyhow!(
                        "invalid hash length (expected {}, was {})",
                        H256::len_bytes(),
                        h.len()
                    ))
                }
            })
            .collect::<Result<Vec<_>, _>>()
    }

    async fn import_transactions(&self, txs: Vec<Vec<u8>>) -> Result<Vec<ImportResult>> {
        let req = ImportRequest { txs };
        let results = self
            .client
            .clone()
            .import_transactions(req)
            .await?
            .into_inner()
            .imported
            .into_iter()
            .map(ImportResult::from_i32)
            .map(|r| r.unwrap_or(ImportResult::InternalError))
            .collect();
        Ok(results)
    }

    async fn get_transactions(&self, hashes: &[H256]) -> Result<Vec<Vec<u8>>> {
        let req = GetTransactionsRequest {
            hashes: hashes.iter().map(|hash| hash.as_bytes().to_vec()).collect(),
        };

        let res = self
            .client
            .clone()
            .get_transactions(req)
            .await?
            .into_inner()
            .txs;

        Ok(res)
    }
}

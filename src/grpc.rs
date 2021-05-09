use crate::eth::EthMessageId;
use anyhow::bail;
use std::convert::TryFrom;

pub use ethereum_interfaces::sentry;

impl TryFrom<EthMessageId> for sentry::MessageId {
    type Error = anyhow::Error;

    fn try_from(id: EthMessageId) -> Result<Self, Self::Error> {
        Ok(match id {
            EthMessageId::NewBlockHashes => Self::NewBlockHashes,
            EthMessageId::GetBlockHeaders => Self::GetBlockHeaders,
            EthMessageId::BlockHeaders => Self::BlockHeaders,
            EthMessageId::GetBlockBodies => Self::GetBlockBodies,
            EthMessageId::BlockBodies => Self::BlockBodies,
            EthMessageId::NewBlock => Self::NewBlock,
            EthMessageId::GetNodeData => Self::GetNodeData,
            EthMessageId::NodeData => Self::NodeData,
            other => bail!("Invalid message id: {:?}", other),
        })
    }
}

impl From<sentry::MessageId> for EthMessageId {
    fn from(id: sentry::MessageId) -> Self {
        match id {
            sentry::MessageId::NewBlockHashes => Self::NewBlockHashes,
            sentry::MessageId::NewBlock => Self::NewBlock,
            sentry::MessageId::Transactions => Self::Transactions,
            sentry::MessageId::NewPooledTransactionHashes => Self::NewPooledTransactionHashes,
            sentry::MessageId::GetBlockHeaders => Self::GetBlockHeaders,
            sentry::MessageId::GetBlockBodies => Self::GetBlockBodies,
            sentry::MessageId::GetNodeData => Self::GetNodeData,
            sentry::MessageId::GetReceipts => Self::GetReceipts,
            sentry::MessageId::GetPooledTransactions => Self::GetPooledTransactions,
            sentry::MessageId::BlockHeaders => Self::BlockHeaders,
            sentry::MessageId::BlockBodies => Self::BlockBodies,
            sentry::MessageId::NodeData => Self::NodeData,
            sentry::MessageId::Receipts => Self::Receipts,
            sentry::MessageId::PooledTransactions => Self::PooledTransactions,
        }
    }
}

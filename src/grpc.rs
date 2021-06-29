use crate::eth::{EthMessageId, EthProtocolVersion};
use anyhow::bail;
use std::convert::TryFrom;

pub use ethereum_interfaces::sentry;

impl TryFrom<EthMessageId> for sentry::MessageId {
    type Error = anyhow::Error;

    fn try_from(id: EthMessageId) -> Result<Self, Self::Error> {
        Ok(match id {
            EthMessageId::NewBlockHashes => Self::NewBlockHashes66,
            EthMessageId::GetBlockHeaders => Self::GetBlockHeaders66,
            EthMessageId::BlockHeaders => Self::BlockHeaders66,
            EthMessageId::GetBlockBodies => Self::GetBlockBodies66,
            EthMessageId::BlockBodies => Self::BlockBodies66,
            EthMessageId::NewBlock => Self::NewBlock66,
            EthMessageId::GetNodeData => Self::GetNodeData66,
            EthMessageId::NodeData => Self::NodeData66,
            other => bail!("Invalid message id: {:?}", other),
        })
    }
}

impl TryFrom<sentry::MessageId> for EthMessageId {
    type Error = anyhow::Error;

    fn try_from(id: sentry::MessageId) -> Result<Self, Self::Error> {
        Ok(match id {
            sentry::MessageId::NewBlockHashes66 => Self::NewBlockHashes,
            sentry::MessageId::NewBlock66 => Self::NewBlock,
            sentry::MessageId::Transactions66 => Self::Transactions,
            sentry::MessageId::NewPooledTransactionHashes66 => Self::NewPooledTransactionHashes,
            sentry::MessageId::GetBlockHeaders66 => Self::GetBlockHeaders,
            sentry::MessageId::GetBlockBodies66 => Self::GetBlockBodies,
            sentry::MessageId::GetNodeData66 => Self::GetNodeData,
            sentry::MessageId::GetReceipts66 => Self::GetReceipts,
            sentry::MessageId::GetPooledTransactions66 => Self::GetPooledTransactions,
            sentry::MessageId::BlockHeaders66 => Self::BlockHeaders,
            sentry::MessageId::BlockBodies66 => Self::BlockBodies,
            sentry::MessageId::NodeData66 => Self::NodeData,
            sentry::MessageId::Receipts66 => Self::Receipts,
            sentry::MessageId::PooledTransactions66 => Self::PooledTransactions,
            other => bail!("Unsupported message id: {:?}", other),
        })
    }
}

impl From<EthProtocolVersion> for sentry::Protocol {
    fn from(version: EthProtocolVersion) -> Self {
        match version {
            EthProtocolVersion::Eth65 => Self::Eth65,
            EthProtocolVersion::Eth66 => Self::Eth66,
        }
    }
}

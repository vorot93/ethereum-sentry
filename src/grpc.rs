use crate::eth::MessageId;
use anyhow::bail;
use std::convert::TryFrom;

pub mod txpool {
    tonic::include_proto!("txpool");
}

pub mod control {
    tonic::include_proto!("control");
}

pub mod sentry {
    tonic::include_proto!("sentry");
}

impl TryFrom<MessageId> for control::InboundMessageId {
    type Error = anyhow::Error;

    fn try_from(id: MessageId) -> Result<Self, Self::Error> {
        Ok(match id {
            MessageId::NewBlockHashes => Self::NewBlockHashes,
            MessageId::BlockHeaders => Self::BlockHeaders,
            MessageId::BlockBodies => Self::BlockBodies,
            MessageId::NewBlock => Self::NewBlock,
            MessageId::NodeData => Self::NodeData,
            other => bail!("Invalid message id: {:?}", other),
        })
    }
}

impl MessageId {
    pub fn from_outbound_message_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::GetBlockHeaders,
            1 => Self::GetBlockBodies,
            2 => Self::GetNodeData,
            _ => return None,
        })
    }
}

impl From<sentry::OutboundMessageId> for MessageId {
    fn from(id: sentry::OutboundMessageId) -> Self {
        match id {
            sentry::OutboundMessageId::GetBlockHeaders => Self::GetBlockHeaders,
            sentry::OutboundMessageId::GetBlockBodies => Self::GetBlockBodies,
            sentry::OutboundMessageId::GetNodeData => Self::GetNodeData,
        }
    }
}

use crate::{NodeId, NodeRecord};
use arrayref::array_ref;
use primitive_types::H256;
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rlp_derive::{RlpDecodable, RlpEncodable};
use std::net::IpAddr;

impl Encodable for NodeRecord {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(4);
        match self.address {
            IpAddr::V4(v) => {
                s.append(&v.octets().as_ref());
            }
            IpAddr::V6(v) => {
                s.append(&v.octets().as_ref());
            }
        };
        s.append(&self.udp_port);
        s.append(&self.tcp_port);
        s.append(&self.id);
    }
}

impl Decodable for NodeRecord {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        let address_raw = rlp.at(0)?.data()?;
        let address = match address_raw.len() {
            4 => IpAddr::from(*array_ref!(address_raw, 0, 4)),
            16 => IpAddr::from(*array_ref!(address_raw, 0, 16)),
            _ => {
                return Err(DecoderError::Custom("wrong address length"));
            }
        };
        Ok(Self {
            address,
            udp_port: rlp.val_at(1)?,
            tcp_port: rlp.val_at(2)?,
            id: rlp.val_at(3)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub address: IpAddr,
    pub udp_port: u16,
    pub tcp_port: u16,
}

impl Encodable for Endpoint {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(3);
        match self.address {
            IpAddr::V4(v) => {
                s.append(&v.octets().as_ref());
            }
            IpAddr::V6(v) => {
                s.append(&v.octets().as_ref());
            }
        };
        s.append(&self.udp_port);
        s.append(&self.tcp_port);
    }
}

impl Decodable for Endpoint {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        let address_raw = rlp.at(0)?.data()?;
        let address = match address_raw.len() {
            4 => IpAddr::from(*array_ref!(address_raw, 0, 4)),
            16 => IpAddr::from(*array_ref!(address_raw, 0, 16)),
            _ => {
                return Err(DecoderError::Custom("wrong address length"));
            }
        };
        Ok(Self {
            address,
            udp_port: rlp.val_at(1)?,
            tcp_port: rlp.val_at(2)?,
        })
    }
}

impl From<NodeRecord> for Endpoint {
    fn from(
        NodeRecord {
            address,
            tcp_port,
            udp_port,
            ..
        }: NodeRecord,
    ) -> Self {
        Self {
            address,
            udp_port,
            tcp_port,
        }
    }
}

#[derive(Clone, Copy, Debug, RlpEncodable, RlpDecodable)]
pub struct FindNodeMessage {
    pub id: NodeId,
    pub expire: u64,
}

#[derive(Clone, Debug, RlpEncodable, RlpDecodable)]
pub struct NeighboursMessage {
    pub nodes: Vec<NodeRecord>,
    pub expire: u64,
}

#[derive(Debug, Clone)]
pub struct PingMessage {
    pub from: Endpoint,
    pub to: Endpoint,
    pub expire: u64,
}

impl Encodable for PingMessage {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(4);
        s.append(&4_u32); // Version 4
        s.append(&self.from);
        s.append(&self.to);
        s.append(&self.expire);
    }
}

impl Decodable for PingMessage {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        Ok(Self {
            from: rlp.val_at(1)?,
            to: rlp.val_at(2)?,
            expire: rlp.val_at(3)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PongMessage {
    pub to: Endpoint,
    pub echo: H256,
    pub expire: u64,
}

impl Encodable for PongMessage {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(3);
        s.append(&self.to);
        s.append(&self.echo);
        s.append(&self.expire);
    }
}

impl Decodable for PongMessage {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        Ok(Self {
            to: rlp.val_at(0)?,
            echo: rlp.val_at(1)?,
            expire: rlp.val_at(2)?,
        })
    }
}

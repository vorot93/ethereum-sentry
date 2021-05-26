#![allow(dead_code)]

use arrayvec::ArrayString;
use async_stream::stream;
use async_trait::async_trait;
use devp2p::*;
use ethereum_types::*;
use futures::stream::BoxStream;
use hex_literal::hex;
use maplit::btreemap;
use parking_lot::RwLock;
use rlp_derive::{RlpDecodable, RlpEncodable};
use secp256k1::SecretKey;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use task_group::*;
use tokio::{
    sync::{
        mpsc::{channel, Sender},
        Mutex as AsyncMutex,
    },
    time::sleep,
};
use tokio_stream::{StreamExt, StreamMap};
use tracing::*;
use tracing_subscriber::EnvFilter;

const DISCV4_BOOTNODES: &[&str] = &[
    "enode://d860a01f9722d78051619d1e2351aba3f43f943f6f00718d1b9baa4101932a1f5011f16bb2b1bb35db20d6fe28fa0bf09636d26a87d31de9ec6203eeedb1f666@18.138.108.67:30303",
    "enode://22a8232c3abc76a16ae9d6c3b164f98775fe226f0917b0ca871128a74a8e9630b458460865bab457221f1d448dd9791d24c4e5d88786180ac185df813a68d4de@3.209.45.79:30303",
    "enode://ca6de62fce278f96aea6ec5a2daadb877e51651247cb96ee310a318def462913b653963c155a0ef6c7d50048bba6e6cea881130857413d9f50a621546b590758@34.255.23.113:30303",
    "enode://279944d8dcd428dffaa7436f25ca0ca43ae19e7bcf94a8fb7d1641651f92d121e972ac2e8f381414b80cc8e5555811c2ec6e1a99bb009b3f53c4c69923e11bd8@35.158.244.151:30303",
    "enode://8499da03c47d637b20eee24eec3c356c9a2e6148d6fe25ca195c7949ab8ec2c03e3556126b0d7ed644675e78c4318b08691b7b57de10e5f0d40d05b09238fa0a@52.187.207.27:30303",
    "enode://103858bdb88756c71f15e9b5e09b56dc1be52f0a5021d46301dbbfb7e130029cc9d0d6f73f693bc29b665770fff7da4d34f3c6379fe12721b5d7a0bcb5ca1fc1@191.234.162.198:30303",
    "enode://715171f50508aba88aecd1250af392a45a330af91d7b90701c436b618c86aaa1589c9184561907bebbb56439b8f8787bc01f49a7c77276c58c1b09822d75e8e8@52.231.165.108:30303",
    "enode://5d6d7cd20d6da4bb83a1d28cadb5d409b64edf314c0335df658c1a54e32c7c4a7ab7823d57c39b6a757556e68ff1df17c748b698544a55cb488b52479a92b60f@104.42.217.25:30303",
    "enode://68f46370191198b71a1595dd453c489bbfe28036a9951fc0397fabd1b77462930b3c5a5359b20e99677855939be47b39fc8edcf1e9ff2522a922b86d233bf2df@144.217.153.76:30303",
    "enode://ffed6382e05ee42854d862f08e4e39b8452c50a5a5d399072c40f9a0b2d4ad34b0eb5312455ad8bcf0dcb4ce969dc89a9a9fd00183eaf8abf46bbcc59dc6e9d5@51.195.3.238:30303",
    "enode://b47b197244c054d385f25d7740b33cc7e2a74d6f715befad2b789fd3e3594bb1c8dd2ca2faf1a3bf6b4c9ec03e53b52301f722a2316b78976be03ccbe703c581@54.37.94.238:30303",
    "enode://5f7d0794c464b2fcd514d41e16e4b535a98ac792a71ca9667c7cef35595dc34c9a1b793c0622554cf87f34006942abb526af7d2e37d715ac32ed02170556cce2@51.161.101.207:30303",
];

fn eth() -> CapabilityName {
    CapabilityName(ArrayString::from("eth").unwrap())
}

#[derive(Debug, Default)]
struct TaskMetrics {
    count: AtomicUsize,
}

impl task_group::Metrics for TaskMetrics {
    fn task_started(&self, id: TaskId, name: String) {
        let c = self.count.fetch_add(1, Ordering::Relaxed);
        trace!("TASK+ | {} total | {} | id: {}", c + 1, name, id)
    }

    fn task_stopped(&self, id: TaskId, name: String) {
        let c = self.count.fetch_sub(1, Ordering::Relaxed);
        trace!("TASK- | {} total | {} | id: {}", c - 1, name, id)
    }
}

#[derive(Debug, RlpEncodable, RlpDecodable)]
struct StatusMessage {
    protocol_version: usize,
    network_id: usize,
    total_difficulty: U256,
    best_hash: H256,
    genesis_hash: H256,
}

#[derive(Clone)]
struct Pipes {
    sender: Sender<OutboundEvent>,
    receiver: Arc<AsyncMutex<BoxStream<'static, OutboundEvent>>>,
}

#[derive(Default)]
struct CapabilityServerImpl {
    peer_pipes: Arc<RwLock<HashMap<PeerId, Pipes>>>,
}

impl CapabilityServerImpl {
    fn setup_pipes(&self, peer: PeerId, pipes: Pipes) {
        assert!(self.peer_pipes.write().insert(peer, pipes).is_none());
    }
    fn get_pipes(&self, peer: PeerId) -> Pipes {
        self.peer_pipes.read().get(&peer).unwrap().clone()
    }
    fn teardown(&self, peer: PeerId) {
        self.peer_pipes.write().remove(&peer);
    }
    fn connected_peers(&self) -> usize {
        self.peer_pipes.read().len()
    }
}

#[async_trait]
impl CapabilityServer for CapabilityServerImpl {
    #[instrument(skip(self, peer), fields(peer=&*peer.to_string()))]
    fn on_peer_connect(&self, peer: PeerId, caps: HashMap<CapabilityName, CapabilityVersion>) {
        info!("Setting up peer state");
        let status_message = StatusMessage {
            protocol_version: *caps.get(&eth()).unwrap(),
            network_id: 1,
            total_difficulty: 17608636743620256866935_u128.into(),
            best_hash: H256::from(hex!(
                "28042e7e4d35a3482bf5f0d862501868b04c1734f483ceae3bf1393561951829"
            )),
            genesis_hash: H256::from(hex!(
                "d4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3"
            )),
        };

        let first_message = OutboundEvent::Message {
            capability_name: eth(),
            message: Message {
                id: 0,
                data: rlp::encode(&status_message).into(),
            },
        };

        let (sender, mut receiver) = channel(1);

        self.setup_pipes(
            peer,
            Pipes {
                sender,
                receiver: Arc::new(AsyncMutex::new(Box::pin(stream! {
                    yield first_message;

                    while let Some(message) = receiver.recv().await {
                        yield message;
                    }
                }))),
            },
        );
    }
    #[instrument(skip(self, peer, event), fields(peer=&*peer.to_string(), event=&*event.to_string()))]
    async fn on_peer_event(&self, peer: PeerId, event: InboundEvent) {
        match event {
            InboundEvent::Disconnect { .. } => {
                self.teardown(peer);
            }
            InboundEvent::Message { message, .. } => {
                info!(
                    "Received message with id {}, data {}",
                    message.id,
                    hex::encode(&message.data)
                );

                if message.id == 0 {
                    match rlp::decode::<StatusMessage>(&message.data) {
                        Ok(v) => {
                            info!("Decoded status message: {:?}", v);
                        }
                        Err(e) => {
                            info!("Failed to decode status message: {}! Kicking peer.", e);
                            let _ = self
                                .get_pipes(peer)
                                .sender
                                .send(OutboundEvent::Disconnect {
                                    reason: DisconnectReason::ProtocolBreach,
                                })
                                .await;

                            return;
                        }
                    }
                }

                let out_id = match message.id {
                    3 => Some(4),
                    5 => Some(6),
                    _ => None,
                };

                if let Some(id) = out_id {
                    let _ = self
                        .get_pipes(peer)
                        .sender
                        .send(OutboundEvent::Message {
                            capability_name: eth(),
                            message: Message {
                                id,
                                data: rlp::encode_list::<String, String>(&[]).into(),
                            },
                        })
                        .await;
                }
            }
        }
    }
    #[instrument(skip(self, peer), fields(peer=&*peer.to_string()))]
    async fn next(&self, peer: PeerId) -> OutboundEvent {
        let outbound = self
            .get_pipes(peer)
            .receiver
            .lock()
            .await
            .next()
            .await
            .unwrap_or(OutboundEvent::Disconnect {
                reason: DisconnectReason::DisconnectRequested,
            });

        info!("Sending outbound event {:?}", outbound);

        outbound
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let secret_key = SecretKey::new(&mut secp256k1::rand::thread_rng());

    let task_metrics = Arc::new(TaskMetrics::default());
    let task_group = Arc::new(TaskGroup::new_with_metrics(task_metrics.clone()));

    let port = 30303;

    let mut discovery_tasks = StreamMap::new();
    discovery_tasks.insert(
        "discv4".to_string(),
        Box::pin(
            Discv4Builder::default()
                .with_cache(20)
                .with_concurrent_lookups(50)
                .build(
                    discv4::Node::new(
                        format!("0.0.0.0:{}", port).parse().unwrap(),
                        SecretKey::new(&mut secp256k1::rand::thread_rng()),
                        DISCV4_BOOTNODES
                            .iter()
                            .map(|v| v.parse().unwrap())
                            .collect(),
                        None,
                        true,
                        port,
                    )
                    .await
                    .unwrap(),
                ),
        ) as Discovery,
    );

    let capability_server = Arc::new(CapabilityServerImpl::default());

    let swarm = Swarm::builder()
        .with_task_group(task_group.clone())
        .with_listen_options(ListenOptions {
            discovery_tasks,
            max_peers: 50,
            addr: format!("0.0.0.0:{}", port).parse().unwrap(),
            cidr: None,
            no_new_peers: Default::default(),
        })
        .build(
            btreemap! { CapabilityId {
                name: eth(),
                version: 63,
            } => 17 },
            capability_server,
            secret_key,
        )
        .await
        .unwrap();

    loop {
        sleep(std::time::Duration::from_secs(5)).await;
        info!("Peers: {}.", swarm.connected_peers());
    }
}

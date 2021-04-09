#![allow(dead_code, clippy::upper_case_acronyms)]

use crate::{
    config::*,
    eth::*,
    grpc::sentry::{sentry_server::SentryServer, InboundMessage},
    services::*,
};
use anyhow::{anyhow, Context};
use async_stream::stream;
use async_trait::async_trait;
use clap::Clap;
use devp2p::*;
use educe::Educe;
use futures::stream::BoxStream;
use grpc::sentry;
use maplit::btreemap;
use num_traits::{FromPrimitive, ToPrimitive};
use parking_lot::RwLock;
use secp256k1::{PublicKey, SecretKey, SECP256K1};
use std::{
    collections::{btree_map::Entry, hash_map::Entry as HashMapEntry, BTreeMap, HashMap, HashSet},
    convert::TryFrom,
    fmt::Debug,
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use task_group::TaskGroup;
use tokio::{
    sync::{
        broadcast::{channel as broadcast, Sender as BroadcastSender},
        mpsc::{channel, Sender},
        Mutex as AsyncMutex,
    },
    time::sleep,
};
use tokio_stream::{StreamExt, StreamMap};
use tonic::transport::Server;
use tracing::*;
use tracing_subscriber::EnvFilter;
use trust_dns_resolver::{config::*, TokioAsyncResolver};

mod config;
mod eth;
mod grpc;
mod services;
mod types;

type OutboundSender = Sender<OutboundEvent>;
type OutboundReceiver = Arc<AsyncMutex<BoxStream<'static, OutboundEvent>>>;

pub const BUFFERING_FACTOR: usize = 5;

#[derive(Clone)]
struct Pipes {
    sender: OutboundSender,
    receiver: OutboundReceiver,
}

#[derive(Clone, Debug, Default)]
struct BlockTracker {
    block_by_peer: HashMap<PeerId, u64>,
    peers_by_block: BTreeMap<u64, HashSet<PeerId>>,
}

impl BlockTracker {
    fn set_block_number(&mut self, peer: PeerId, block: u64, force_create: bool) {
        match self.block_by_peer.entry(peer) {
            HashMapEntry::Vacant(e) => {
                if force_create {
                    e.insert(block);
                } else {
                    return;
                }
            }
            HashMapEntry::Occupied(mut e) => {
                let old_block = std::mem::replace(e.get_mut(), block);
                if let Entry::Occupied(mut entry) = self.peers_by_block.entry(old_block) {
                    entry.get_mut().remove(&peer);

                    if entry.get().is_empty() {
                        entry.remove();
                    }
                }
            }
        }

        self.peers_by_block.entry(block).or_default().insert(peer);
    }

    fn remove_peer(&mut self, peer: PeerId) {
        if let Some(block) = self.block_by_peer.remove(&peer) {
            if let Entry::Occupied(mut entry) = self.peers_by_block.entry(block) {
                entry.get_mut().remove(&peer);

                if entry.get().is_empty() {
                    entry.remove();
                }
            }
        }
    }

    fn peers_with_min_block(&self, block: u64) -> HashSet<PeerId> {
        self.peers_by_block
            .range(block..)
            .map(|(_, v)| v)
            .flatten()
            .copied()
            .collect()
    }
}

#[derive(Educe)]
#[educe(Debug)]
pub struct CapabilityServerImpl {
    #[educe(Debug(ignore))]
    peer_pipes: Arc<RwLock<HashMap<PeerId, Pipes>>>,
    block_tracker: Arc<RwLock<BlockTracker>>,

    status_message: Arc<RwLock<Option<FullStatusData>>>,
    valid_peers: Arc<RwLock<HashSet<PeerId>>>,

    data_sender: BroadcastSender<InboundMessage>,
    upload_requests_sender: BroadcastSender<InboundMessage>,
    tx_message_sender: BroadcastSender<InboundMessage>,
}

impl CapabilityServerImpl {
    fn setup_peer(&self, peer: PeerId, p: Pipes) {
        let mut pipes = self.peer_pipes.write();
        let mut block_tracker = self.block_tracker.write();

        assert!(pipes.insert(peer, p).is_none());
        block_tracker.set_block_number(peer, 0, true);
    }
    fn get_pipes(&self, peer: PeerId) -> Option<Pipes> {
        self.peer_pipes.read().get(&peer).cloned()
    }
    pub fn sender(&self, peer: PeerId) -> Option<OutboundSender> {
        self.peer_pipes
            .read()
            .get(&peer)
            .map(|pipes| pipes.sender.clone())
    }
    fn receiver(&self, peer: PeerId) -> Option<OutboundReceiver> {
        self.peer_pipes
            .read()
            .get(&peer)
            .map(|pipes| pipes.receiver.clone())
    }
    fn teardown_peer(&self, peer: PeerId) {
        let mut pipes = self.peer_pipes.write();
        let mut block_tracker = self.block_tracker.write();
        let mut valid_peers = self.valid_peers.write();

        pipes.remove(&peer);
        block_tracker.remove_peer(peer);
        valid_peers.remove(&peer);
    }

    pub fn all_peers(&self) -> HashSet<PeerId> {
        self.peer_pipes.read().keys().copied().collect()
    }

    pub fn connected_peers(&self) -> usize {
        self.valid_peers.read().len()
    }

    #[instrument(skip(self))]
    async fn handle_event(
        &self,
        peer: PeerId,
        event: InboundEvent,
    ) -> Result<Option<Message>, DisconnectReason> {
        match event {
            InboundEvent::Disconnect { reason } => {
                debug!("Peer disconnect (reason: {:?}), tearing down peer.", reason);
                self.teardown_peer(peer);
            }
            InboundEvent::Message {
                message: Message { id, data },
                ..
            } => {
                let valid_peer = self.valid_peers.read().contains(&peer);
                let message_id = EthMessageId::from_usize(id);
                match message_id {
                    None => {
                        debug!("Unknown message");
                    }
                    Some(EthMessageId::Status) => {
                        let v = rlp::decode::<StatusMessage>(&data).map_err(|e| {
                            debug!("Failed to decode status message: {}! Kicking peer.", e);

                            DisconnectReason::ProtocolBreach
                        })?;

                        debug!("Decoded status message: {:?}", v);

                        let status_data = self.status_message.read();
                        let mut valid_peers = self.valid_peers.write();
                        if let Some(FullStatusData { fork_filter, .. }) = &*status_data {
                            fork_filter.validate(v.fork_id).map_err(|reason| {
                                debug!("Kicking peer with incompatible fork ID: {:?}", reason);

                                DisconnectReason::UselessPeer
                            })?;

                            valid_peers.insert(peer);
                        }
                    }
                    Some(inbound_id) if valid_peer => {
                        if let Some(sender) = match inbound_id {
                            EthMessageId::BlockBodies
                            | EthMessageId::BlockHeaders
                            | EthMessageId::NodeData => Some(&self.data_sender),
                            EthMessageId::GetBlockBodies
                            | EthMessageId::GetBlockHeaders
                            | EthMessageId::GetNodeData => Some(&self.upload_requests_sender),
                            // EthMessageId::Transactions
                            // | EthMessageId::NewPooledTransactionHashes
                            // | EthMessageId::GetPooledTransactions
                            // | EthMessageId::PooledTransactions => Some(&self.tx_message_sender),
                            _ => None,
                        } {
                            if sender
                                .send(InboundMessage {
                                    id: sentry::MessageId::try_from(inbound_id).unwrap() as i32,
                                    data,
                                    peer_id: Some(peer.into()),
                                })
                                .is_err()
                            {
                                warn!("no connected sentry, dropping status and peer");
                                *self.status_message.write() = None;

                                return Err(DisconnectReason::ClientQuitting);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(None)
    }
}

#[async_trait]
impl CapabilityServer for CapabilityServerImpl {
    #[instrument(skip(self, peer), level = "debug", fields(peer=&*peer.to_string()))]
    fn on_peer_connect(&self, peer: PeerId, caps: HashMap<CapabilityName, CapabilityVersion>) {
        let first_events = if let Some(FullStatusData {
            status,
            fork_filter,
        }) = &*self.status_message.read()
        {
            let status_message = StatusMessage {
                protocol_version: *caps
                    .get(&capability_name())
                    .expect("peer without this cap would have been disconnected"),
                network_id: status.network_id,
                total_difficulty: status.total_difficulty,
                best_hash: status.best_hash,
                genesis_hash: status.fork_data.genesis,
                fork_id: fork_filter.current(),
            };

            vec![OutboundEvent::Message {
                capability_name: capability_name(),
                message: Message {
                    id: EthMessageId::Status.to_usize().unwrap(),
                    data: rlp::encode(&status_message).into(),
                },
            }]
        } else {
            vec![OutboundEvent::Disconnect {
                reason: DisconnectReason::DisconnectRequested,
            }]
        };

        let (sender, mut receiver) = channel(1);
        self.setup_peer(
            peer,
            Pipes {
                sender,
                receiver: Arc::new(AsyncMutex::new(Box::pin(stream! {
                    for event in first_events {
                        yield event;
                    }

                    while let Some(event) = receiver.recv().await {
                        yield event;
                    }
                }))),
            },
        );
    }
    #[instrument(skip(self, peer, event), level = "debug", fields(peer=&*peer.to_string(), event=&*event.to_string()))]
    async fn on_peer_event(&self, peer: PeerId, event: InboundEvent) {
        debug!("Received message");

        if let Some(ev) = self.handle_event(peer, event).await.transpose() {
            let _ = self
                .sender(peer)
                .unwrap()
                .send(match ev {
                    Ok(message) => OutboundEvent::Message {
                        capability_name: capability_name(),
                        message,
                    },
                    Err(reason) => OutboundEvent::Disconnect { reason },
                })
                .await;
        }
    }

    #[instrument(skip(self, peer), level = "debug", fields(peer=&*peer.to_string()))]
    async fn next(&self, peer: PeerId) -> OutboundEvent {
        self.receiver(peer)
            .unwrap()
            .lock()
            .await
            .next()
            .await
            .unwrap_or(OutboundEvent::Disconnect {
                reason: DisconnectReason::DisconnectRequested,
            })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            if std::env::var(EnvFilter::DEFAULT_ENV)
                .unwrap_or_default()
                .is_empty()
            {
                EnvFilter::new(
                    "ethereum_sentry=info,devp2p=info,discv4=info,discv5=info,dnsdisc=info",
                )
            } else {
                EnvFilter::from_default_env()
            },
        )
        .init();

    let opts = Opts::parse();

    let secret_key;
    if let Some(data) = opts.node_key {
        secret_key = SecretKey::from_slice(&hex::decode(data)?)?;
        info!("Loaded node key from config");
    } else {
        secret_key = SecretKey::new(&mut secp256k1::rand::thread_rng());
        info!("Generated new node key: {}", secret_key);
    };

    let listen_addr = format!("0.0.0.0:{}", opts.listen_port);

    info!("Starting Ethereum sentry");

    info!(
        "Node ID: {}",
        hex::encode(
            devp2p::util::pk2id(&PublicKey::from_secret_key(SECP256K1, &secret_key)).as_bytes()
        )
    );

    if let Some(cidr_filter) = &opts.cidr {
        info!("Peers restricted to range {}", cidr_filter);
    }

    let mut discovery_tasks = StreamMap::new();

    info!("Starting DNS discovery fetch from {}", opts.dnsdisc_address);
    let dns_resolver = dnsdisc::Resolver::new(Arc::new(
        TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default())
            .context("Failed to start DNS resolver")?,
    ));

    discovery_tasks.insert(
        "dnsdisc".to_string(),
        Box::pin(DnsDiscovery::new(
            Arc::new(dns_resolver),
            opts.dnsdisc_address,
            None,
        )) as Discovery,
    );

    info!("Starting discv4 at port {}", opts.discv4_port);

    let mut bootstrap_nodes = opts
        .discv4_bootnodes
        .into_iter()
        .map(|Discv4NR(nr)| nr)
        .collect::<Vec<_>>();

    if bootstrap_nodes.is_empty() {
        bootstrap_nodes = BOOTNODES
            .iter()
            .map(|b| Ok(Discv4NR::from_str(b)?.0))
            .collect::<Result<Vec<_>, <Discv4NR as FromStr>::Err>>()?;
        info!("Using default discv4 bootstrap nodes");
    }
    discovery_tasks.insert(
        "discv4".to_string(),
        Box::pin(
            Discv4Builder::default()
                .with_cache(opts.discv4_cache)
                .with_concurrent_lookups(opts.discv4_concurrent_lookups)
                .build(
                    discv4::Node::new(
                        format!("0.0.0.0:{}", opts.discv4_port).parse().unwrap(),
                        secret_key,
                        bootstrap_nodes,
                        None,
                        true,
                        opts.listen_port,
                    )
                    .await
                    .unwrap(),
                ),
        ),
    );

    if opts.discv5 {
        let addr = opts
            .discv5_addr
            .ok_or_else(|| anyhow!("no discv5 addr specified"))?;
        let enr = opts
            .discv5_enr
            .ok_or_else(|| anyhow!("discv5 ENR not specified"))?;

        let mut svc = discv5::Discv5::new(
            enr,
            discv5::enr::CombinedKey::Secp256k1(
                k256::ecdsa::SigningKey::from_bytes(secret_key.as_ref()).unwrap(),
            ),
            Default::default(),
        )
        .map_err(|e| anyhow!("{}", e))?;
        svc.start(addr.parse()?)
            .await
            .map_err(|e| anyhow!("{}", e))
            .context("Failed to start discv5")?;
        info!("Starting discv5 at {}", addr);

        for bootnode in opts.discv5_bootnodes {
            svc.add_enr(bootnode).unwrap();
        }
        discovery_tasks.insert("discv5".to_string(), Box::pin(Discv5::new(svc, 20)));
    }

    if !opts.reserved_peers.is_empty() {
        info!("Enabling reserved peers: {:?}", opts.reserved_peers);
        discovery_tasks.insert(
            "reserved peers".to_string(),
            Box::pin(Bootnodes::from(
                opts.reserved_peers
                    .iter()
                    .map(|&NR(NodeRecord { addr, id })| (addr, id))
                    .collect::<HashMap<_, _>>(),
            )),
        );
    }

    let tasks = Arc::new(TaskGroup::new());

    let data_sender = broadcast(opts.max_peers * BUFFERING_FACTOR).0;
    let upload_requests_sender = broadcast(opts.max_peers * BUFFERING_FACTOR).0;
    let tx_message_sender = broadcast(opts.max_peers * BUFFERING_FACTOR).0;
    let capability_server = Arc::new(CapabilityServerImpl {
        peer_pipes: Default::default(),
        block_tracker: Default::default(),
        status_message: Default::default(),
        valid_peers: Default::default(),
        data_sender,
        upload_requests_sender,
        tx_message_sender,
    });

    let swarm = Swarm::builder()
        .with_task_group(tasks.clone())
        .with_listen_options(ListenOptions {
            discovery_tasks,
            max_peers: opts.max_peers,
            addr: listen_addr.parse().unwrap(),
            cidr: opts.cidr,
        })
        .with_client_version(format!("sentry/v{}", env!("CARGO_PKG_VERSION")))
        .build(
            btreemap! {
                CapabilityId { name: capability_name(), version: 65 } => 17,
            },
            capability_server.clone(),
            secret_key,
        )
        .await
        .context("Failed to start RLPx node")?;

    info!("RLPx node listening at {}", listen_addr);

    let sentry_addr = opts.sentry_addr.parse()?;
    tasks.spawn(async move {
        let svc = SentryServer::new(SentryService::new(capability_server));

        info!("Sentry gRPC server starting on {}", sentry_addr);

        Server::builder()
            .add_service(svc)
            .serve(sentry_addr)
            .await
            .unwrap();
    });

    loop {
        info!(
            "Peer info: {} active (+{} dialing) / {} max.",
            swarm.connected_peers(),
            swarm.dialing(),
            opts.max_peers
        );

        sleep(Duration::from_secs(5)).await;
    }
}

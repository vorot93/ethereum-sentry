#![allow(dead_code, clippy::upper_case_acronyms)]

use crate::{
    config::*,
    eth::*,
    grpc::{
        control::{InboundMessage, InboundMessageId},
        sentry::sentry_server::SentryServer,
        txpool::ImportResult,
    },
    services::*,
};
use anyhow::{anyhow, Context};
use async_stream::stream;
use async_trait::async_trait;
use bytes::Bytes;
use clap::Clap;
use devp2p::*;
use educe::Educe;
use ethereum_forkid::ForkFilter;
use ethereum_types::*;
use futures::stream::BoxStream;
use maplit::btreemap;
use num_traits::{FromPrimitive, ToPrimitive};
use parking_lot::RwLock;
use rlp::Rlp;
use secp256k1::{PublicKey, SecretKey, SECP256K1};
use std::{
    collections::{btree_map::Entry, BTreeMap, HashMap, HashSet},
    convert::TryFrom,
    fmt::Debug,
    sync::Arc,
    time::Duration,
};
use task_group::TaskGroup;
use tokio::{
    sync::{
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
    fn set_block_number(&mut self, peer: PeerId, block: u64) {
        if let Some(old_block) = self.block_by_peer.insert(peer, block) {
            if let Entry::Occupied(mut entry) = self.peers_by_block.entry(old_block) {
                entry.get_mut().remove(&peer);

                if entry.get().is_empty() {
                    entry.remove();
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
pub struct CapabilityServerImpl<C, DP, TxPool>
where
    C: Debug,
    DP: Debug,
    TxPool: Debug,
{
    #[educe(Debug(ignore))]
    peer_pipes: Arc<RwLock<HashMap<PeerId, Pipes>>>,
    block_tracker: Arc<RwLock<BlockTracker>>,

    status_message: Arc<RwLock<Option<(StatusData, ForkFilter)>>>,
    valid_peers: Arc<RwLock<HashSet<PeerId>>>,
    control: C,
    data_provider: DP,
    txpool: TxPool,
}

impl<C: Control, DP: DataProvider, TxPool: Txpool> CapabilityServerImpl<C, DP, TxPool> {
    fn setup_peer(&self, peer: PeerId, p: Pipes) {
        let mut pipes = self.peer_pipes.write();
        let mut block_tracker = self.block_tracker.write();

        assert!(pipes.insert(peer, p).is_none());
        block_tracker.set_block_number(peer, 0);
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
                let message_id = MessageId::from_usize(id);
                match message_id {
                    None => {
                        debug!("Unknown message");
                    }
                    Some(MessageId::Status) => {
                        let v = rlp::decode::<StatusMessage>(&data).map_err(|e| {
                            debug!("Failed to decode status message: {}! Kicking peer.", e);

                            DisconnectReason::ProtocolBreach
                        })?;

                        debug!("Decoded status message: {:?}", v);

                        let status_data = self.status_message.read();
                        let mut valid_peers = self.valid_peers.write();
                        if let Some((_, fork_filter)) = &*status_data {
                            fork_filter.validate(v.fork_id).map_err(|reason| {
                                debug!("Kicking peer with incompatible fork ID: {:?}", reason);

                                DisconnectReason::UselessPeer
                            })?;

                            valid_peers.insert(peer);
                        }
                    }
                    Some(MessageId::GetBlockHeaders) if valid_peer => {
                        let selector = rlp::decode::<GetBlockHeaders>(&*data)
                            .map_err(|_| DisconnectReason::ProtocolBreach)?;
                        debug!("Requested headers: {:?}", selector);

                        let selector = async {
                            let anchor = match selector.block {
                                BlockId::Number(num) => num,
                                BlockId::Hash(hash) => {
                                    match self.data_provider.resolve_block_height(hash).await {
                                        Ok(Some(height)) => height,
                                        Ok(None) => {
                                            // this block does not exist, exit early.
                                            return vec![];
                                        }
                                        Err(e) => {
                                            debug!("Failed to resolve block {}: {}", hash, e);
                                            return vec![];
                                        }
                                    }
                                }
                            };

                            std::iter::empty()
                                .chain((0..selector.max_headers).map(|i| {
                                    let skip = selector.skip + 1;
                                    if selector.reverse {
                                        anchor - skip * i
                                    } else {
                                        anchor + skip * i
                                    }
                                }))
                                .collect()
                        }
                        .await;

                        let mut block_headers = self.data_provider.get_block_headers(
                            selector.iter().copied().map(BlockId::Number).collect(),
                        );

                        let mut output = Vec::with_capacity(selector.len());
                        while let Some(res) = block_headers.next().await {
                            match res {
                                Ok(v) => output.push(v),
                                Err(e) => {
                                    debug!(
                                        "Failed to get block header: {}, stopping header query",
                                        e
                                    );
                                    break;
                                }
                            }
                        }

                        let id = MessageId::BlockHeaders;
                        debug!("Replying: {:?} / {} headers", id, output.len());

                        return Ok(Some(Message {
                            id: id.to_usize().unwrap(),
                            data: rlp::encode_list(&output).into(),
                        }));
                    }
                    Some(MessageId::GetBlockBodies) if valid_peer => {
                        let blocks = Rlp::new(&*data)
                            .as_list()
                            .map_err(|_| DisconnectReason::ProtocolBreach)?;
                        debug!("Requested {} block bodies", blocks.len());

                        let output: Vec<_> = self
                            .data_provider
                            .get_block_bodies(blocks)
                            .filter_map(|res| match res {
                                Err(e) => {
                                    debug!("{:?}", e);
                                    None
                                }
                                Ok(v) => Some(v),
                            })
                            .collect::<Vec<_>>()
                            .await;

                        let id = MessageId::BlockBodies;
                        debug!("Replying: {:?} / {} block bodies", id, output.len());

                        return Ok(Some(Message {
                            id: id.to_usize().unwrap(),
                            data: rlp::encode_list(&output).into(),
                        }));
                    }
                    Some(MessageId::BlockHeaders)
                    | Some(MessageId::BlockBodies)
                    | Some(MessageId::NewBlock)
                    | Some(MessageId::NewBlockHashes)
                        if valid_peer =>
                    {
                        let _ = self
                            .control
                            .forward_inbound_message(InboundMessage {
                                id: InboundMessageId::try_from(message_id.unwrap()).unwrap() as i32,
                                data: data.to_vec(),
                                peer_id: peer.as_fixed_bytes().to_vec(),
                            })
                            .await;
                    }
                    Some(MessageId::NewPooledTransactionHashes) if valid_peer => {
                        debug!("NewPooledTransactionHashes");
                        let hashes = Rlp::new(&*data)
                            .as_list::<H256>()
                            .map_err(|_| DisconnectReason::ProtocolBreach)?;

                        let rsp = self.txpool.find_unknown_transactions(&hashes).await;
                        let mut txs = match rsp {
                            Ok(t) => t,
                            Err(e) => {
                                warn!("Finding unknown transactions failed: {}", e);
                                return Ok(None);
                            }
                        };

                        txs.truncate(256); // TODO: Send multiple messages to get all txs.

                        let txs: Vec<_> =
                            txs.into_iter().map(|tx| tx.as_bytes().to_vec()).collect();

                        let reply = Message {
                            id: MessageId::GetPooledTransactions as usize,
                            data: rlp::encode_list::<Vec<u8>, _>(&txs).into(),
                        };

                        return Ok(Some(reply));
                    }
                    Some(MessageId::PooledTransactions) if valid_peer => {
                        debug!("PooledTransactions");
                        let txs = Rlp::new(&*data)
                            .iter()
                            .map(|r| r.as_raw().to_vec())
                            .collect();

                        let results = match self.txpool.import_transactions(txs).await {
                            Err(e) => {
                                warn!("Importing transactions failed: {}", e);
                                return Ok(None);
                            }
                            Ok(r) => r,
                        };

                        let mut success = 0;
                        let mut exists = 0;
                        let mut fee = 0;
                        let mut stale = 0;
                        let mut invalid = 0;
                        let mut error = 0;

                        for result in results {
                            match result {
                                ImportResult::Success => success += 1,
                                ImportResult::AlreadyExists => exists += 1,
                                ImportResult::FeeTooLow => fee += 1,
                                ImportResult::Stale => stale += 1,
                                ImportResult::Invalid => invalid += 1,
                                ImportResult::InternalError => error += 1,
                            }
                        }

                        info!(
                            "Imported transactions: success={} exists={} fee={} stale={} invalid={} error={}",
                            success,
                            exists,
                            fee,
                            stale,
                            invalid,
                            error,
                        );
                    }
                    Some(MessageId::GetPooledTransactions) if valid_peer => {
                        let mut out = Bytes::from_static(&rlp::EMPTY_LIST_RLP);
                        let hashes = Rlp::new(&*data)
                            .as_list::<H256>()
                            .map_err(|_| DisconnectReason::ProtocolBreach)?;

                        if let Ok(rsp) = self.txpool.get_transactions(&hashes).await {
                            out = rlp::encode_list::<Vec<u8>, _>(&rsp).into();
                        }

                        return Ok(Some(Message {
                            id: MessageId::PooledTransactions.to_usize().unwrap(),
                            data: out,
                        }));
                    }
                    _ => {}
                }
            }
        }

        Ok(None)
    }
}

#[async_trait]
impl<C, DP, TxPool> CapabilityServer for CapabilityServerImpl<C, DP, TxPool>
where
    C: Control,
    DP: DataProvider,
    TxPool: Txpool,
{
    #[instrument(skip(self, peer), level = "debug", fields(peer=&*peer.to_string()))]
    fn on_peer_connect(&self, peer: PeerId, caps: HashMap<CapabilityName, CapabilityVersion>) {
        let first_events = if let Some((status_data, fork_filter)) = &*self.status_message.read() {
            let status_message = StatusMessage {
                protocol_version: *caps
                    .get(&capability_name())
                    .expect("peer without this cap would have been disconnected"),
                network_id: status_data.network_id,
                total_difficulty: status_data.total_difficulty,
                best_hash: status_data.best_hash,
                genesis_hash: status_data.fork_data.genesis,
                fork_id: fork_filter.current(),
            };

            vec![OutboundEvent::Message {
                capability_name: capability_name(),
                message: Message {
                    id: MessageId::Status.to_usize().unwrap(),
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
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let opts =
        toml::from_str::<Config>(&std::fs::read_to_string(Opts::parse().config_path).unwrap())
            .unwrap();

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

    if let Some(dnsdisc_opts) = opts.dnsdisc {
        info!("Starting DNS discovery fetch from {}", dnsdisc_opts.address);
        let dns_resolver = dnsdisc::Resolver::new(Arc::new(TokioAsyncResolver::tokio(
            ResolverConfig::default(),
            ResolverOpts::default(),
        )?));

        discovery_tasks.insert(
            "dnsdisc".to_string(),
            Box::pin(DnsDiscovery::new(
                Arc::new(dns_resolver),
                dnsdisc_opts.address,
                None,
            )) as Discovery,
        );
    }

    if let Some(discv4_opts) = opts.discv4 {
        info!("Starting discv4 at port {}", discv4_opts.port);

        let bootstrap_nodes = discv4_opts
            .bootnodes
            .into_iter()
            .map(|Dicv4NR(nr)| nr)
            .collect::<Vec<_>>();

        if bootstrap_nodes.is_empty() {
            warn!("discv4 cannot work without bootstrap nodes!");
        }
        discovery_tasks.insert(
            "discv4".to_string(),
            Box::pin(
                Discv4Builder::default()
                    .with_cache(discv4_opts.cache)
                    .with_concurrent_lookups(discv4_opts.concurrent_lookups)
                    .build(
                        discv4::Node::new(
                            format!("0.0.0.0:{}", discv4_opts.port).parse().unwrap(),
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
    }

    if let Some(discv5_opts) = opts.discv5 {
        let mut svc = discv5::Discv5::new(
            discv5_opts
                .enr
                .ok_or_else(|| anyhow!("discv5 ENR not specified"))?,
            discv5::enr::CombinedKey::Secp256k1(
                k256::ecdsa::SigningKey::from_bytes(secret_key.as_ref()).unwrap(),
            ),
            Default::default(),
        )
        .map_err(|e| anyhow!("{}", e))?;
        svc.start(discv5_opts.addr.parse()?)
            .await
            .map_err(|e| anyhow!("{}", e))
            .context("Failed to start discv5")?;
        info!("Starting discv5 at {}", discv5_opts.addr);

        for bootnode in discv5_opts.bootnodes {
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

    let data_provider: Arc<dyn DataProvider> = match opts.data_provider {
        DataProviderSettings::Dummy => Arc::new(DummyDataProvider),
        DataProviderSettings::Tarpc { addr } => {
            let addr = addr.to_string();
            info!("Using tarpc data provider at {}", addr);
            Arc::new(
                TarpcDataProvider::new(addr)
                    .await
                    .context("Failed to start tarpc data provider")?,
            )
        }
    };
    let control: Arc<dyn Control> = if let Some(addr) = opts.control_addr {
        Arc::new(GrpcControl::connect(addr.to_string()).await?)
    } else {
        Arc::new(DummyControl)
    };

    let txpool: Arc<dyn Txpool> = match opts.txpool_addr {
        Some(addr) => Arc::new(GrpcTxpool::connect(addr.to_string()).await?),
        None => Arc::new(DummyTxpool),
    };

    let status_message: Arc<RwLock<Option<(StatusData, ForkFilter)>>> = Default::default();

    tasks.spawn_with_name("Status updater", {
        let status_message = status_message.clone();
        let control = control.clone();
        let data_provider = data_provider.clone();
        async move {
            loop {
                match async {
                    let status_data = match control.get_status_data().await {
                        Err(e) => {
                            debug!(
                                "Failed to get status from control, trying from data provider: {}",
                                e
                            );
                            match data_provider.get_status_data().await {
                                Err(e) => {
                                    debug!("Failed to fetch status from data provider: {}", e);
                                    return Err(e);
                                }
                                Ok(v) => v,
                            }
                        }
                        Ok(v) => v,
                    };

                    debug!("Resolving best hash");
                    let best_block = data_provider
                        .resolve_block_height(status_data.best_hash)
                        .await
                        .context("failed to resolve best hash")?
                        .ok_or_else(|| anyhow!("invalid best hash"))?;

                    let fork_filter = ForkFilter::new(
                        best_block,
                        status_data.fork_data.genesis,
                        status_data.fork_data.forks.iter().copied(),
                    );

                    Ok((status_data, fork_filter))
                }
                .await
                {
                    Ok(s) => {
                        debug!("Setting status data to {:?}", s);
                        *status_message.write() = Some(s);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to fetch status data: {}. Server will not accept new peers.",
                            e
                        );
                        *status_message.write() = None;
                    }
                }

                sleep(Duration::from_secs(5)).await;
            }
        }
    });

    let capability_server = Arc::new(CapabilityServerImpl {
        peer_pipes: Default::default(),
        block_tracker: Default::default(),
        status_message,
        valid_peers: Default::default(),
        control,
        data_provider,
        txpool,
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
        .await?;

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

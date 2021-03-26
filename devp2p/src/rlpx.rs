//! RLPx protocol implementation in Rust

use crate::{disc::Discovery, node_filter::*, peer::*, transport::Transport, types::*};
use anyhow::{anyhow, bail, Context};
use cidr::{Cidr, IpCidr};
use educe::Educe;
use futures::sink::SinkExt;
use parking_lot::Mutex;
use secp256k1::SecretKey;
use std::{
    collections::{hash_map::Entry, BTreeMap, HashMap, HashSet},
    fmt::Debug,
    future::Future,
    net::SocketAddr,
    ops::Deref,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Weak,
    },
    time::Duration,
};
use task_group::TaskGroup;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{
        mpsc::{channel, unbounded_channel},
        oneshot::{channel as oneshot, Sender as OneshotSender},
    },
    time::sleep,
};
use tokio_stream::{StreamExt, StreamMap};
use tracing::*;
use uuid::Uuid;

const GRACE_PERIOD_SECS: u64 = 2;
const HANDSHAKE_TIMEOUT_SECS: u64 = 10;
const PING_TIMEOUT: Duration = Duration::from_secs(60);
const DISCOVERY_TIMEOUT_SECS: u64 = 90;
const DISCOVERY_CONNECT_TIMEOUT_SECS: u64 = 5;
const DIAL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy)]
enum DisconnectInitiator {
    Local,
    LocalForceful,
    Remote,
}

struct DisconnectSignal {
    initiator: DisconnectInitiator,
    reason: DisconnectReason,
}

#[derive(Debug)]
struct ConnectedPeerState {
    tasks: TaskGroup,
}

#[derive(Debug)]
enum PeerState {
    Connecting { connection_id: Uuid },
    Connected(ConnectedPeerState),
}

impl PeerState {
    const fn is_connected(&self) -> bool {
        matches!(self, Self::Connected(_))
    }
}

#[derive(Debug)]
struct PeerStreams {
    /// Mapping of remote IDs to streams in `StreamMap`
    mapping: HashMap<PeerId, PeerState>,
}

impl PeerStreams {
    fn disconnect_peer(&mut self, remote_id: PeerId) -> bool {
        debug!("disconnecting peer {}", remote_id);

        self.mapping.remove(&remote_id).is_some()
    }
}

impl Default for PeerStreams {
    fn default() -> Self {
        Self {
            mapping: HashMap::new(),
        }
    }
}

#[derive(Educe)]
#[educe(Clone)]
struct PeerStreamHandshakeData<C> {
    port: u16,
    secret_key: SecretKey,
    client_version: String,
    capabilities: Arc<CapabilitySet>,
    capability_server: Arc<C>,
}

async fn handle_incoming<C>(
    task_group: Weak<TaskGroup>,
    streams: Arc<Mutex<PeerStreams>>,
    node_filter: Arc<Mutex<dyn NodeFilter>>,
    tcp_incoming: TcpListener,
    cidr: Option<IpCidr>,
    handshake_data: PeerStreamHandshakeData<C>,
) where
    C: CapabilityServer,
{
    let _: anyhow::Result<()> = async {
        loop {
            match tcp_incoming.accept().await {
                Err(e) => {
                    bail!("failed to accept peer: {:?}, shutting down", e);
                }
                Ok((stream, remote_addr)) => {
                    let tasks = task_group
                        .upgrade()
                        .ok_or_else(|| anyhow!("task group is down"))?;

                    if let Some(cidr) = &cidr {
                        if !cidr.contains(&remote_addr.ip()) {
                            debug!(
                                "Ignoring connection request: {} is not in range {}",
                                remote_addr, cidr
                            );

                            continue;
                        }
                    }

                    let f = handle_incoming_request(
                        streams.clone(),
                        node_filter.clone(),
                        stream,
                        handshake_data.clone(),
                    );
                    tasks.spawn_with_name(format!("Incoming connection setup: {}", remote_addr), f);
                }
            }
        }
    }
    .await;
}

/// Set up newly connected peer's state, start its tasks
fn setup_peer_state<C, Io>(
    streams: Weak<Mutex<PeerStreams>>,
    capability_server: Arc<C>,
    remote_id: PeerId,
    peer: PeerStream<Io>,
) -> ConnectedPeerState
where
    C: CapabilityServer,
    Io: Transport,
{
    let capability_set = peer
        .capabilities()
        .iter()
        .copied()
        .map(|cap_info| (cap_info.name, cap_info.version))
        .collect::<HashMap<_, _>>();
    let (mut sink, mut stream) = futures::StreamExt::split(peer);
    let (peer_disconnect_tx, mut peer_disconnect_rx) = unbounded_channel();
    let tasks = TaskGroup::default();

    capability_server.on_peer_connect(remote_id, capability_set);

    let pinged = Arc::new(AtomicBool::default());
    let (pings_tx, mut pings) = channel(1);
    let (pongs_tx, mut pongs) = channel(1);

    tasks.spawn_with_name(format!("peer {} ingress router", remote_id), {
        let peer_disconnect_tx = peer_disconnect_tx.clone();
        let capability_server = capability_server.clone();
        let pinged = pinged.clone();
        async move {
            let disconnect_signal = {
                async move {
                    while let Some(message) = stream.next().await {
                        match message {
                            Err(e) => {
                                debug!("Peer incoming error: {}", e);
                                break;
                            }
                            Ok(PeerMessage::Subprotocol(SubprotocolMessage {
                                cap_name,
                                message,
                            })) => {
                                // Actually handle the message
                                capability_server
                                    .on_peer_event(
                                        remote_id,
                                        InboundEvent::Message {
                                            capability_name: cap_name,
                                            message,
                                        },
                                    )
                                    .await
                            }
                            Ok(PeerMessage::Disconnect(reason)) => {
                                // Peer has requested disconnection.
                                return DisconnectSignal {
                                    initiator: DisconnectInitiator::Remote,
                                    reason,
                                };
                            }
                            Ok(PeerMessage::Ping) => {
                                let _ = pongs_tx.send(()).await;
                            }
                            Ok(PeerMessage::Pong) => {
                                pinged.store(false, Ordering::Relaxed);
                            }
                        }
                    }

                    // Ingress stream is closed, force disconnect the peer.
                    DisconnectSignal {
                        initiator: DisconnectInitiator::Remote,
                        reason: DisconnectReason::DisconnectRequested,
                    }
                }
            }
            .await;

            let _ = peer_disconnect_tx.send(disconnect_signal);
        }
        .instrument(span!(Level::DEBUG, "IN", "peer={}", remote_id.to_string(),))
    });

    tasks.spawn_with_name(
        format!("peer {} egress router & disconnector", remote_id),
        async move {
            let mut event_fut = capability_server.next(remote_id);
            loop {
                let mut disconnecting = None;
                let mut egress = None;
                let mut trigger: Option<OneshotSender<()>> = None;
                tokio::select! {
                    // Event from capability server.
                    msg = &mut event_fut => {
                        // Invariant: CapabilityServer::next() will never be called after disconnect event
                        match msg {
                            OutboundEvent::Message {
                                capability_name, message
                            } => {
                                event_fut = capability_server.next(remote_id);
                                egress = Some(PeerMessage::Subprotocol(SubprotocolMessage {
                                    cap_name: capability_name, message
                                }));
                            }
                            OutboundEvent::Disconnect {
                                reason
                            } => {
                                egress = Some(PeerMessage::Disconnect(reason));
                                disconnecting = Some(DisconnectSignal {
                                    initiator: DisconnectInitiator::Local, reason
                                });
                            }
                        };
                    },
                    // We ping the peer.
                    Some(tx) = pings.recv() => {
                        egress = Some(PeerMessage::Ping);
                        trigger = Some(tx);
                    }
                    // Peer has pinged us.
                    Some(_) = pongs.recv() => {
                        egress = Some(PeerMessage::Pong);
                    }
                    // Ping timeout or signal from ingress router.
                    Some(DisconnectSignal { initiator, reason }) = peer_disconnect_rx.recv() => {
                        if let DisconnectInitiator::Local = initiator {
                            egress = Some(PeerMessage::Disconnect(reason));
                        }
                        disconnecting = Some(DisconnectSignal { initiator, reason })
                    }
                };

                if let Some(message) = egress {
                    trace!("Sending message: {:?}", message);

                    // Send egress message, force disconnect on error.
                    if let Err(e) = sink.send(message).await {
                        debug!("peer disconnected with error {:?}", e);
                        disconnecting.get_or_insert(DisconnectSignal {
                            initiator: DisconnectInitiator::LocalForceful,
                            reason: DisconnectReason::TcpSubsystemError,
                        });
                    } else if let Some(trigger) = trigger {
                        let _ = trigger.send(());
                    }
                }

                if let Some(DisconnectSignal { initiator, reason }) = disconnecting {
                    if let DisconnectInitiator::Local = initiator {
                        // We have sent disconnect message, wait for grace period.
                        sleep(Duration::from_secs(GRACE_PERIOD_SECS)).await;
                    }
                    capability_server
                        .on_peer_event(
                            remote_id,
                            InboundEvent::Disconnect {
                                reason: Some(reason),
                            },
                        )
                        .await;
                    break;
                }
            }

            // We are done, drop the peer state.
            if let Some(streams) = streams.upgrade() {
                // This is the last line that is guaranteed to be executed.
                // After this the peer's task group is dropped and any alive tasks are forcibly cancelled.
                streams.lock().disconnect_peer(remote_id);
            }
        }
        .instrument(span!(
            Level::DEBUG,
            "OUT/DISC",
            "peer={}",
            remote_id.to_string(),
        )),
    );

    tasks.spawn_with_name(format!("peer {} pinger", remote_id), async move {
        loop {
            pinged.store(true, Ordering::Relaxed);

            let (tx, rx) = oneshot();
            if pings_tx.send(tx).await.is_ok() && rx.await.is_ok() {
                sleep(PING_TIMEOUT).await;

                if pinged.load(Ordering::Relaxed) {
                    let _ = peer_disconnect_tx.send(DisconnectSignal {
                        initiator: DisconnectInitiator::Local,
                        reason: DisconnectReason::PingTimeout,
                    });

                    return;
                }

                continue;
            }

            return;
        }
    });
    ConnectedPeerState { tasks }
}

/// Establishes the connection with peer and adds them to internal state.
async fn handle_incoming_request<C, Io>(
    streams: Arc<Mutex<PeerStreams>>,
    node_filter: Arc<Mutex<dyn NodeFilter>>,
    stream: Io,
    handshake_data: PeerStreamHandshakeData<C>,
) where
    C: CapabilityServer,
    Io: Transport,
{
    let PeerStreamHandshakeData {
        secret_key,
        client_version,
        capabilities,
        capability_server,
        port,
    } = handshake_data;
    // Do handshake and convert incoming connection into stream.
    let peer_res = tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        PeerStream::incoming(
            stream,
            secret_key,
            client_version,
            capabilities.get_capabilities().to_vec(),
            port,
        ),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow!("incoming connection timeout")));

    match peer_res {
        Ok(peer) => {
            let remote_id = peer.remote_id();
            let s = streams.clone();
            let mut s = s.lock();
            let node_filter = node_filter.clone();
            let PeerStreams { mapping } = &mut *s;
            let total_connections = mapping.len();

            match mapping.entry(remote_id) {
                Entry::Occupied(entry) => {
                    debug!(
                        "We are already {} to remote peer {}!",
                        if entry.get().is_connected() {
                            "connected"
                        } else {
                            "connecting"
                        },
                        remote_id
                    );
                }
                Entry::Vacant(entry) => {
                    if node_filter.lock().allow(total_connections, remote_id) {
                        debug!("New incoming peer connected: {}", remote_id);
                        entry.insert(PeerState::Connected(setup_peer_state(
                            Arc::downgrade(&streams),
                            capability_server,
                            remote_id,
                            peer,
                        )));
                    } else {
                        trace!("Node filter rejected peer {}, disconnecting", remote_id);
                    }
                }
            }
        }
        Err(e) => {
            debug!("Peer disconnected with error {}", e);
        }
    }
}

#[derive(Debug, Default)]
struct CapabilitySet {
    inner: BTreeMap<CapabilityId, CapabilityLength>,

    capability_cache: Vec<CapabilityInfo>,
}

impl CapabilitySet {
    fn get_capabilities(&self) -> &[CapabilityInfo] {
        &self.capability_cache
    }
}

impl From<BTreeMap<CapabilityId, CapabilityLength>> for CapabilitySet {
    fn from(inner: BTreeMap<CapabilityId, CapabilityLength>) -> Self {
        let capability_cache = inner
            .iter()
            .map(
                |(&CapabilityId { name, version }, &length)| CapabilityInfo {
                    name,
                    version,
                    length,
                },
            )
            .collect();

        Self {
            inner,
            capability_cache,
        }
    }
}

/// This is an asynchronous RLPx server implementation.
///
/// `Swarm` is the representation of swarm of connected RLPx peers
/// supports registration for capability servers.
///
/// This implementation is based on the concept of structured concurrency.
/// Internal state is managed by a multitude of workers that run in separate runtime tasks
/// spawned on the running executor during the server creation and addition of new peers.
/// All continuously running workers are inside the task scope owned by the server struct.
#[derive(Educe)]
#[educe(Debug)]
pub struct Swarm<C: CapabilityServer> {
    #[allow(unused)]
    tasks: Arc<TaskGroup>,

    streams: Arc<Mutex<PeerStreams>>,

    currently_connecting: Arc<AtomicUsize>,

    node_filter: Arc<Mutex<dyn NodeFilter>>,

    capabilities: Arc<CapabilitySet>,
    #[educe(Debug(ignore))]
    capability_server: Arc<C>,

    #[educe(Debug(ignore))]
    secret_key: SecretKey,
    client_version: String,
    port: u16,
}

/// Builder for ergonomically creating a new `Server`.
#[derive(Debug)]
pub struct SwarmBuilder {
    task_group: Option<Arc<TaskGroup>>,
    listen_options: Option<ListenOptions>,
    client_version: String,
}

impl SwarmBuilder {
    pub fn with_task_group(mut self, task_group: Arc<TaskGroup>) -> Self {
        self.task_group = Some(task_group);
        self
    }

    pub fn with_listen_options(mut self, options: ListenOptions) -> Self {
        self.listen_options = Some(options);
        self
    }

    pub fn with_client_version(mut self, version: String) -> Self {
        self.client_version = version;
        self
    }

    /// Create a new RLPx node
    pub async fn build<C: CapabilityServer>(
        self,
        capability_mask: BTreeMap<CapabilityId, CapabilityLength>,
        capability_server: Arc<C>,
        secret_key: SecretKey,
    ) -> anyhow::Result<Arc<Swarm<C>>> {
        Swarm::new_inner(
            secret_key,
            self.client_version,
            self.task_group,
            capability_mask.into(),
            capability_server,
            self.listen_options,
        )
        .await
    }
}

#[derive(Educe)]
#[educe(Debug)]
pub struct ListenOptions {
    #[educe(Debug(ignore))]
    pub discovery_tasks: StreamMap<String, Discovery>,
    pub max_peers: usize,
    pub addr: SocketAddr,
    pub cidr: Option<IpCidr>,
}

impl Swarm<()> {
    pub fn builder() -> SwarmBuilder {
        SwarmBuilder {
            task_group: None,
            listen_options: None,
            client_version: format!("rust-devp2p/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

impl<C: CapabilityServer> Swarm<C> {
    pub async fn new(
        capability_mask: BTreeMap<CapabilityId, CapabilityLength>,
        capability_server: Arc<C>,
        secret_key: SecretKey,
    ) -> anyhow::Result<Arc<Self>> {
        Swarm::builder()
            .build(capability_mask, capability_server, secret_key)
            .await
    }

    async fn new_inner(
        secret_key: SecretKey,
        client_version: String,
        task_group: Option<Arc<TaskGroup>>,
        capabilities: CapabilitySet,
        capability_server: Arc<C>,
        listen_options: Option<ListenOptions>,
    ) -> anyhow::Result<Arc<Self>> {
        let tasks = task_group.unwrap_or_default();

        let port = listen_options
            .as_ref()
            .map_or(0, |options| options.addr.port());

        let streams = Arc::new(Mutex::new(PeerStreams::default()));
        let node_filter = Arc::new(Mutex::new(MemoryNodeFilter::new(Arc::new(
            listen_options
                .as_ref()
                .map_or(0.into(), |options| options.max_peers.into()),
        ))));

        let capabilities = Arc::new(capabilities);

        if let Some(options) = &listen_options {
            let tcp_incoming = TcpListener::bind(options.addr)
                .await
                .context("Failed to bind RLPx node to socket")?;
            let cidr = options.cidr.clone();
            tasks.spawn_with_name(
                "incoming handler",
                handle_incoming(
                    Arc::downgrade(&tasks),
                    streams.clone(),
                    node_filter.clone(),
                    tcp_incoming,
                    cidr,
                    PeerStreamHandshakeData {
                        port,
                        secret_key,
                        client_version: client_version.clone(),
                        capabilities: capabilities.clone(),
                        capability_server: capability_server.clone(),
                    },
                ),
            );
        }

        let server = Arc::new(Self {
            tasks: tasks.clone(),
            streams,
            currently_connecting: Default::default(),
            node_filter,
            capabilities,
            capability_server,
            secret_key,
            client_version,
            port,
        });

        if let Some(mut options) = listen_options {
            tasks.spawn_with_name("dialer", {
                let server = Arc::downgrade(&server);
                let tasks = Arc::downgrade(&tasks);
                async move {
                    let current_peers = Arc::new(Mutex::new(HashSet::new()));
                    loop {
                        if let Some(server) = server.upgrade() {
                            let streams_len = server.streams.lock().mapping.len();
                            let max_peers = server.node_filter.lock().max_peers();

                            if streams_len < max_peers {
                                trace!("Discovering peers as our peer count is too low: {} < {}", streams_len, max_peers);
                                match tokio::time::timeout(
                                    Duration::from_secs(DISCOVERY_TIMEOUT_SECS),
                                    options.discovery_tasks.next(),
                                )
                                .await {
                                    Err(_) => {
                                        debug!("Failed to get new peer: timed out");
                                    }
                                    Ok(None) => {
                                        debug!("Discoveries ended, dialer quitting");
                                        return;
                                    }
                                    Ok(Some((disc_id, Ok(NodeRecord { addr, id: remote_id })))) => {
                                        if let Some(tasks) = tasks.upgrade() {
                                            if current_peers.lock().insert(remote_id) {
                                                debug!("Discovered peer: {:?} ({})", remote_id, disc_id);
                                                tasks.spawn_with_name(format!("add peer {} at {}", remote_id, addr), {
                                                    let current_peers = current_peers.clone();
                                                    async move {
                                                        if tokio::time::timeout(
                                                            Duration::from_secs(DISCOVERY_CONNECT_TIMEOUT_SECS),
                                                            server.add_peer_inner(addr, remote_id, true)
                                                        ).await.is_err() {
                                                            debug!("Timed out adding peer {}", remote_id);
                                                        }
                                                        current_peers.lock().remove(&remote_id)
                                                    }
                                                });
                                            }
                                        }
                                    }
                                    Ok(Some((disc_id, Err(e)))) => warn!("Failed to get new peer: {} ({})", e, disc_id)
                                }

                                sleep(DIAL_INTERVAL).await;
                            } else {
                                trace!("Skipping discovery as current number of peers is too high: {} >= {}", streams_len, max_peers);
                                sleep(Duration::from_secs(2)).await;
                            }
                        } else {
                            return;
                        }
                    }
                }.instrument(span!(Level::DEBUG, "dialer"))
            });
        }

        Ok(server)
    }

    /// Add a new peer to this RLPx node. Returns `true` if it was added successfully (did not exist before, accepted by node filter).
    pub fn add_peer(
        &self,
        node_record: NodeRecord,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send + 'static {
        self.add_peer_inner(node_record.addr, node_record.id, false)
    }

    fn add_peer_inner(
        &self,
        addr: SocketAddr,
        remote_id: PeerId,
        check_peer: bool,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send + 'static {
        let tasks = self.tasks.clone();
        let streams = self.streams.clone();
        let node_filter = self.node_filter.clone();

        let capabilities = self.capabilities.clone();
        let capability_set = capabilities.get_capabilities().to_vec();
        let capability_server = self.capability_server.clone();

        let secret_key = self.secret_key;
        let client_version = self.client_version.clone();
        let port = self.port;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let connection_id = Uuid::new_v4();
        let currently_connecting = self.currently_connecting.clone();

        // Start reaper task that will terminate this connection if connection future gets dropped.
        tasks.spawn_with_name(format!("connection {} reaper", connection_id), {
            let cid = connection_id;
            let streams = streams.clone();
            let currently_connecting = currently_connecting.clone();
            async move {
                if rx.await.is_err() {
                    let mut s = streams.lock();
                    if let Entry::Occupied(entry) = s.mapping.entry(remote_id) {
                        // If this is the same connection attempt, then remove.
                        if let PeerState::Connecting { connection_id } = entry.get() {
                            if *connection_id == cid {
                                trace!("Reaping failed outbound connection: {}/{}", remote_id, cid);

                                entry.remove();
                            }
                        }
                    }
                }
                currently_connecting.fetch_sub(1, Ordering::Relaxed);
            }
        });

        async move {
            trace!("Received request to add peer {}", remote_id);
            let mut inserted = false;

            currently_connecting.fetch_add(1, Ordering::Relaxed);

            {
                let mut streams = streams.lock();
                let node_filter = node_filter.lock();

                let connection_num = streams.mapping.len();

                match streams.mapping.entry(remote_id) {
                    Entry::Occupied(key) => {
                        debug!(
                            "We are already {} to remote peer {}!",
                            if key.get().is_connected() {
                                "connected"
                            } else {
                                "connecting"
                            },
                            remote_id
                        );
                    }
                    Entry::Vacant(vacant) => {
                        if check_peer && !node_filter.allow(connection_num, remote_id) {
                            trace!("rejecting peer {}", remote_id);
                        } else {
                            debug!("connecting to peer {} at {}", remote_id, addr);

                            vacant.insert(PeerState::Connecting { connection_id });
                            inserted = true;
                        }
                    }
                }
            }

            if !inserted {
                return Ok(false);
            }

            // Connecting to peer is a long running operation so we have to break the mutex lock.
            let peer_res = async {
                let transport = TcpStream::connect(addr).await?;
                PeerStream::connect(
                    transport,
                    secret_key,
                    remote_id,
                    client_version,
                    capability_set,
                    port,
                )
                .await
            }
            .await;

            let s = streams.clone();
            let mut s = s.lock();
            let PeerStreams { mapping } = &mut *s;

            // Adopt the new connection if the peer has not been dropped or superseded by incoming connection.
            if let Entry::Occupied(mut peer_state) = mapping.entry(remote_id) {
                if !peer_state.get().is_connected() {
                    match peer_res {
                        Ok(peer) => {
                            assert_eq!(peer.remote_id(), remote_id);
                            debug!("New peer connected: {}", remote_id);

                            *peer_state.get_mut() = PeerState::Connected(setup_peer_state(
                                Arc::downgrade(&streams),
                                capability_server,
                                remote_id,
                                peer,
                            ));

                            let _ = tx.send(());
                            return Ok(true);
                        }
                        Err(e) => {
                            debug!("peer disconnected with error {}", e);
                            peer_state.remove();
                            return Err(e);
                        }
                    }
                }
            }

            Ok(false)
        }
        .instrument(span!(Level::DEBUG, "add peer",))
    }

    /// Returns the number of peers we're currently dialing
    pub fn dialing(&self) -> usize {
        self.currently_connecting.load(Ordering::Relaxed)
    }
}

impl<C: CapabilityServer> Deref for Swarm<C> {
    type Target = C;

    fn deref(&self) -> &Self::Target {
        &*self.capability_server
    }
}

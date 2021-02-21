use arrayvec::ArrayString;
use async_trait::async_trait;
use devp2p::*;
use maplit::btreemap;
use rand::{seq::SliceRandom, thread_rng};
use secp256k1::SecretKey;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::time::sleep;
use tracing::*;
use tracing_subscriber::EnvFilter;

#[derive(Debug)]
struct DummyServer;

#[async_trait]
impl CapabilityServer for DummyServer {
    #[instrument(skip(self, peer), fields(peer=&*peer.to_string()))]
    fn on_peer_connect(&self, peer: PeerId, _: HashMap<CapabilityName, CapabilityVersion>) {
        info!("Peer connected")
    }

    #[instrument(skip(self, _peer, _event), fields(peer=&*_peer.to_string(), event=&*_event.to_string()))]
    async fn on_peer_event(&self, _peer: PeerId, _event: InboundEvent) {
        info!("Received event");
    }

    #[instrument(skip(self, _peer), fields(peer=&*_peer.to_string()))]
    async fn next(&self, _peer: PeerId) -> OutboundEvent {
        futures::future::pending().await
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Bootnodes as used in OpenEthereum
    let bootnodes = vec![
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

    let node = bootnodes
        .choose(&mut thread_rng())
        .unwrap()
        .parse::<NodeRecord>()
        .unwrap();

    debug!("Connecting to {}", node.addr);

    let swarm = Swarm::new(
            btreemap! { CapabilityId { name: CapabilityName(ArrayString::from("eth").unwrap()), version: 63 } => 15 },
            Arc::new(DummyServer),
            SecretKey::new(&mut secp256k1::rand::thread_rng()),
        )
        .await
        .unwrap();

    swarm.add_peer(node).await.unwrap();

    let timeout = 5;
    sleep(Duration::from_secs(timeout)).await;
}

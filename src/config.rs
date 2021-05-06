use cidr::IpCidr;
use clap::Clap;
use derive_more::FromStr;
use devp2p::NodeRecord;
use educe::Educe;
use std::path::PathBuf;

pub const BOOTNODES: &[&str] = &[
	"enode://d860a01f9722d78051619d1e2351aba3f43f943f6f00718d1b9baa4101932a1f5011f16bb2b1bb35db20d6fe28fa0bf09636d26a87d31de9ec6203eeedb1f666@18.138.108.67:30303",   // bootnode-aws-ap-southeast-1-001
	"enode://22a8232c3abc76a16ae9d6c3b164f98775fe226f0917b0ca871128a74a8e9630b458460865bab457221f1d448dd9791d24c4e5d88786180ac185df813a68d4de@3.209.45.79:30303",     // bootnode-aws-us-east-1-001
	"enode://ca6de62fce278f96aea6ec5a2daadb877e51651247cb96ee310a318def462913b653963c155a0ef6c7d50048bba6e6cea881130857413d9f50a621546b590758@34.255.23.113:30303",   // bootnode-aws-eu-west-1-001
	"enode://279944d8dcd428dffaa7436f25ca0ca43ae19e7bcf94a8fb7d1641651f92d121e972ac2e8f381414b80cc8e5555811c2ec6e1a99bb009b3f53c4c69923e11bd8@35.158.244.151:30303",  // bootnode-aws-eu-central-1-001
	"enode://8499da03c47d637b20eee24eec3c356c9a2e6148d6fe25ca195c7949ab8ec2c03e3556126b0d7ed644675e78c4318b08691b7b57de10e5f0d40d05b09238fa0a@52.187.207.27:30303",   // bootnode-azure-australiaeast-001
	"enode://103858bdb88756c71f15e9b5e09b56dc1be52f0a5021d46301dbbfb7e130029cc9d0d6f73f693bc29b665770fff7da4d34f3c6379fe12721b5d7a0bcb5ca1fc1@191.234.162.198:30303", // bootnode-azure-brazilsouth-001
	"enode://715171f50508aba88aecd1250af392a45a330af91d7b90701c436b618c86aaa1589c9184561907bebbb56439b8f8787bc01f49a7c77276c58c1b09822d75e8e8@52.231.165.108:30303",  // bootnode-azure-koreasouth-001
	"enode://5d6d7cd20d6da4bb83a1d28cadb5d409b64edf314c0335df658c1a54e32c7c4a7ab7823d57c39b6a757556e68ff1df17c748b698544a55cb488b52479a92b60f@104.42.217.25:30303",   // bootnode-azure-westus-001
];

#[derive(Educe, Clap)]
#[clap(
    name = "ethereum-sentry",
    about = "Service that listens to Ethereum's P2P network, serves information to other nodes, and provides gRPC interface to clients to interact with the network."
)]
#[educe(Debug)]
pub struct Opts {
    #[clap(long, env)]
    #[educe(Debug(ignore))]
    pub node_key: Option<String>,
    #[clap(long, env, default_value = "30303")]
    pub listen_port: u16,
    #[clap(long, env)]
    pub cidr: Option<IpCidr>,
    #[clap(long, env, default_value = "0.0.0.0:8000")]
    pub sentry_addr: String,
    #[clap(long, env, default_value = "all.mainnet.ethdisco.net")]
    pub dnsdisc_address: String,
    #[clap(long, env, default_value = "30303")]
    pub discv4_port: u16,
    #[clap(long, env)]
    pub discv4_bootnodes: Vec<Discv4NR>,
    #[clap(long, env, default_value = "20")]
    pub discv4_cache: usize,
    #[clap(long, env, default_value = "1")]
    pub discv4_concurrent_lookups: usize,
    #[clap(long, env)]
    pub discv5: bool,
    #[clap(long, env)]
    pub discv5_enr: Option<discv5::Enr>,
    #[clap(long, env)]
    pub discv5_addr: Option<String>,
    #[clap(long, env)]
    pub discv5_bootnodes: Vec<discv5::Enr>,
    #[clap(long, env)]
    pub reserved_peers: Vec<NR>,
    #[clap(long, default_value = "50")]
    pub max_peers: usize,
    #[clap(long, env)]
    pub peers_file: Option<PathBuf>,
    #[clap(long, env)]
    pub tokio_console: bool,
}

#[derive(Debug, Educe)]
#[educe(Default)]
pub struct DnsDiscConfig {
    #[educe(Default("all.mainnet.ethdisco.net"))]
    pub address: String,
}

#[derive(Debug, FromStr)]
pub struct NR(pub NodeRecord);

#[derive(Debug, FromStr)]
pub struct Discv4NR(pub discv4::NodeRecord);

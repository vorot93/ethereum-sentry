use cidr::IpCidr;
use clap::Clap;
use derive_more::FromStr;
use devp2p::NodeRecord;
use educe::Educe;
use serde::Deserialize;
use serde_with::DeserializeFromStr;
use std::path::PathBuf;
use url::Url;

#[derive(Educe, Clap)]
#[clap(
    name = "ethereum-sentry",
    about = "Service that listens to Ethereum's P2P network, serves information to other nodes, and provides gRPC interface to clients to interact with the network."
)]
#[educe(Debug)]
pub struct Opts {
    #[clap(long, env)]
    pub config_path: PathBuf,
}

#[derive(Debug, Deserialize, Educe)]
#[educe(Default)]
pub struct DnsDiscConfig {
    #[educe(Default("all.mainnet.ethdisco.net"))]
    pub address: String,
}

#[derive(Debug, DeserializeFromStr, FromStr)]
pub struct NR(pub NodeRecord);

#[derive(Debug, DeserializeFromStr, FromStr)]
pub struct Dicv4NR(pub discv4::NodeRecord);

#[derive(Debug, Deserialize, Educe)]
#[educe(Default)]
#[serde(default)]
pub struct Discv4Config {
    #[educe(Default(30303))]
    pub port: u16,
    pub bootnodes: Vec<Dicv4NR>,
    #[educe(Default(20))]
    pub cache: usize,
    #[educe(Default(1))]
    pub concurrent_lookups: usize,
}

#[derive(Debug, Deserialize, Educe)]
#[educe(Default)]
#[serde(default)]
pub struct Discv5Config {
    pub enr: Option<discv5::Enr>,
    #[educe(Default("0.0.0.0:30304"))]
    pub addr: String,
    pub bootnodes: Vec<discv5::Enr>,
}

#[derive(Educe, Deserialize)]
#[educe(Default, Debug)]
#[serde(default)]
pub struct Config {
    #[educe(Debug(ignore))]
    pub node_key: Option<String>,
    #[educe(Default(30303))]
    pub listen_port: u16,
    pub cidr: Option<IpCidr>,
    #[educe(Default("0.0.0.0:8000"))]
    pub sentry_addr: String,
    pub dnsdisc: Option<DnsDiscConfig>,
    pub discv4: Option<Discv4Config>,
    pub discv5: Option<Discv5Config>,
    pub reserved_peers: Vec<NR>,
    #[educe(Default(50))]
    pub max_peers: usize,
    pub web3_addr: Option<Url>,
    pub control_addr: Option<Url>,
    pub txpool_addr: Option<Url>,
}

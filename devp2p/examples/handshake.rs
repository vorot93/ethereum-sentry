use devp2p::{ecies::ECIESStream, PeerId};
use hex_literal::hex;
use secp256k1::SecretKey;
use tokio::net::TcpStream;
use tracing_subscriber::EnvFilter;

const REMOTE_ID: PeerId = PeerId(hex!("d860a01f9722d78051619d1e2351aba3f43f943f6f00718d1b9baa4101932a1f5011f16bb2b1bb35db20d6fe28fa0bf09636d26a87d31de9ec6203eeedb1f666"));

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    ECIESStream::connect(
        TcpStream::connect("18.138.108.67:30303").await.unwrap(),
        SecretKey::new(&mut secp256k1::rand::thread_rng()),
        REMOTE_ID,
    )
    .await
    .unwrap();
}

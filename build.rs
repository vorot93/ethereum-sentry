fn config() -> prost_build::Config {
    let mut config = prost_build::Config::new();
    config.bytes(&["."]);
    config
}

fn main() {
    tonic_build::configure()
        .build_client(false)
        .compile_with_config(
            config(),
            &["proto/p2psentry/sentry.proto"],
            &["proto/p2psentry"],
        )
        .unwrap();
}

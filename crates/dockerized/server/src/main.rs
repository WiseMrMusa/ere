use anyhow::{Context, Error};
use clap::Parser;
use ere_server::server::{router, zkVMServer};
use ere_zkvm_interface::zkvm::{ProverResourceType, zkVM};
use std::{
    io::{self, Read},
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tokio::{net::TcpListener, signal};
use tower_http::catch_panic::CatchPanicLayer;
use tracing_subscriber::EnvFilter;
use twirp::{
    Router,
    axum::{self, routing::get},
    reqwest::StatusCode,
    server::not_found_handler,
};

// Compile-time check to ensure exactly one zkVM feature is enabled for `ere-server`
const _: () = {
    if cfg!(feature = "server") {
        assert!(
            (cfg!(feature = "airbender") as u8
                + cfg!(feature = "jolt") as u8
                + cfg!(feature = "miden") as u8
                + cfg!(feature = "nexus") as u8
                + cfg!(feature = "openvm") as u8
                + cfg!(feature = "pico") as u8
                + cfg!(feature = "risc0") as u8
                + cfg!(feature = "sp1") as u8
                + cfg!(feature = "sp1-cluster") as u8
                + cfg!(feature = "ziren") as u8
                + cfg!(feature = "zisk") as u8)
                == 1,
            "Exactly one zkVM feature must be enabled for `ere-server`"
        );
    }
};

#[derive(Parser)]
#[command(author, version)]
struct Args {
    #[arg(long, default_value = "3000")]
    port: u16,
    #[command(subcommand)]
    resource: ProverResourceType,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    // Read serialized program from stdin.
    let mut program = Vec::new();
    io::stdin().read_to_end(&mut program)?;

    let zkvm = construct_zkvm(program, args.resource)?;
    let server = Arc::new(zkVMServer::new(zkvm));
    let app = Router::new()
        .nest("/twirp", router(server))
        .route("/health", get(health))
        .fallback(not_found_handler)
        .layer(CatchPanicLayer::new());

    let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), args.port);
    let tcp_listener = TcpListener::bind(addr).await?;

    tracing::info!("Listening on {}", addr);

    axum::serve(tcp_listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Shutdown gracefully");

    Ok(())
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Received Ctrl+C, shutting down gracefully");
        },
        _ = terminate => {
            tracing::info!("Received SIGTERM, shutting down gracefully");
        },
    }
}

fn construct_zkvm(program: Vec<u8>, resource: ProverResourceType) -> Result<impl zkVM, Error> {
    // Use bincode 1.x to match sp1-cluster's serialization format
    let program: Vec<u8> = bincode1::deserialize(&program)
        .with_context(|| "Failed to deserialize program")?;

    #[cfg(feature = "airbender")]
    let zkvm = ere_airbender::zkvm::EreAirbender::new(program, resource);

    #[cfg(feature = "jolt")]
    let zkvm = ere_jolt::zkvm::EreJolt::new(program, resource);

    #[cfg(feature = "miden")]
    let zkvm = ere_miden::zkvm::EreMiden::new(program, resource);

    #[cfg(feature = "nexus")]
    let zkvm = ere_nexus::zkvm::EreNexus::new(program, resource);

    #[cfg(feature = "openvm")]
    let zkvm = ere_openvm::zkvm::EreOpenVM::new(program, resource);

    #[cfg(feature = "pico")]
    let zkvm = ere_pico::zkvm::ErePico::new(program, resource);

    #[cfg(feature = "risc0")]
    let zkvm = ere_risc0::zkvm::EreRisc0::new(program, resource);

    #[cfg(feature = "sp1")]
    let zkvm = ere_sp1::zkvm::EreSP1::new(program, resource);

    #[cfg(feature = "sp1-cluster")]
    let zkvm = {
        // For SP1 Cluster, the resource should be Cluster, but we also accept CPU/GPU
        // and convert to a ClusterProverConfig
        let cluster_config = match resource {
            ProverResourceType::Cluster(config) => config,
            _ => {
                // Use environment variables for cluster config
                ere_zkvm_interface::zkvm::ClusterProverConfig::default()
            }
        };
        // Wrap Vec<u8> in SP1Program
        let program = ere_sp1_cluster::program::SP1Program::from(program);
        ere_sp1_cluster::zkvm::EreSP1Cluster::new(program, cluster_config)
    };

    #[cfg(feature = "ziren")]
    let zkvm = ere_ziren::zkvm::EreZiren::new(program, resource);

    #[cfg(feature = "zisk")]
    let zkvm = ere_zisk::zkvm::EreZisk::new(program, resource);

    zkvm.with_context(|| "Failed to instantiate zkVM")
}

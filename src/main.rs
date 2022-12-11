extern crate core;

#[forbid(unsafe_code)]
mod client;
mod prover;

use gethostname::gethostname;

use std::{net::ToSocketAddrs, sync::Arc};

use clap::Parser;
use snarkvm::{console::account::address::Address, prelude::Testnet3};

use tracing::{debug, error, info};
use tracing_subscriber::layer::SubscriberExt;

use crate::{
    client::{report, start, Client},
    prover::Prover,
};

#[derive(Debug, Parser)]
#[clap(name = "prover", about = "Standalone prover.")]
struct Opt {
    /// Enable debug logging
    #[clap(short = 'd', long = "debug")]
    debug: bool,

    #[clap(verbatim_doc_comment)]
    /// address
    #[clap(short = 'a', long = "address")]
    address: Option<Address<Testnet3>>,

    /// Beacon node address
    #[clap(short = 'b', long = "beacon")]
    beacon: Option<String>,

    /// Number of threads, defaults to number of CPU threads
    #[clap(short = 't', long = "threads")]
    threads: Option<u16>,

    /// Thread pool size, number of threads in each thread pool, defaults to 4
    #[clap(short = 'i', long = "thread-pool-size")]
    thread_pool_size: Option<u8>,

    /// Output log to file
    #[clap(short = 'o', long = "log")]
    log: Option<String>,

    #[cfg(feature = "cuda")]
    #[clap(short = 'g', long = "cuda")]
    cuda: Option<Vec<i16>>,

    #[cfg(feature = "cuda")]
    #[clap(short = 'j', long = "cuda-jobs")]
    jobs: Option<u8>,

    /// worker, belong to user, can statistics by user and worker
    #[clap(short = 'w', long = "worker")]
    worker: Option<String>,
}

#[tokio::main]
async fn main() {
    #[cfg(windows)]
    let _ = ansi_term::enable_ansi_support();
    dotenvy::dotenv().ok();
    let opt = Opt::parse();
    let tracing_level = if opt.debug {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_max_level(tracing_level)
        .finish();
    if let Some(log) = opt.log {
        let file = std::fs::File::create(log).unwrap();
        let file = tracing_subscriber::fmt::layer()
            .with_writer(file)
            .with_ansi(false);
        tracing::subscriber::set_global_default(subscriber.with(file))
            .expect("unable to set global default subscriber");
    } else {
        tracing::subscriber::set_global_default(subscriber)
            .expect("unable to set global default subscriber");
    }

    let beacons = if opt.beacon.is_none() {
        [
            "164.92.111.59:4133",
            "159.223.204.96:4133",
            "167.71.219.176:4133",
            "157.245.205.209:4133",
            "134.122.95.106:4133",
            "161.35.24.55:4133",
            "138.68.103.139:4133",
            "207.154.215.49:4133",
            "46.101.114.158:4133",
            "138.197.190.94:4133",
        ]
        .map(|s| s.to_string())
        .to_vec()
    } else {
        vec![opt.beacon.unwrap()]
    };
    if opt.address.is_none() {
        error!("Prover address is required!");
        std::process::exit(1);
    }
    let address = opt.address.unwrap();

    beacons
        .iter()
        .map(|s| {
            if let Err(e) = s.to_socket_addrs() {
                error!("Invalid beacon node address: {}", e);
                std::process::exit(1);
            }
        })
        .for_each(drop);

    let threads = opt.threads.unwrap_or(num_cpus::get() as u16);
    let thread_pool_size = opt.thread_pool_size.unwrap_or(4);

    let cuda: Option<Vec<i16>>;
    let cuda_jobs: Option<u8>;
    #[cfg(feature = "cuda")]
    {
        cuda = opt.cuda;
        cuda_jobs = opt.jobs;
    }
    #[cfg(not(feature = "cuda"))]
    {
        cuda = None;
        cuda_jobs = None;
    }
    if let Some(cuda) = cuda.clone() {
        if cuda.is_empty() {
            error!("No GPUs specified. Use -g 0 if there is only one GPU.");
            std::process::exit(1);
        }
    }

    let worker = if opt.worker.is_none() {
        gethostname().into_string().unwrap()
    } else {
        opt.worker.unwrap()
    };

    info!("Starting prover");

    let client = Client::init(address, beacons, worker);

    let prover: Arc<Prover> =
        match Prover::init(threads, thread_pool_size, client.clone(), cuda, cuda_jobs).await {
            Ok(prover) => prover,
            Err(e) => {
                error!("Unable to initialize prover: {}", e);
                std::process::exit(1);
            }
        };
    debug!("Prover initialized");

    start(prover.clone(), client.clone());
    report(prover.clone(), client.clone());

    std::future::pending::<()>().await;
}

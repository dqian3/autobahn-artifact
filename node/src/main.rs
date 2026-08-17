#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]
// Copyright(C) Facebook, Inc. and its affiliates.
use anyhow::{Context, Result};
use bytes::Bytes;
use clap::{crate_name, crate_version, App, AppSettings, ArgMatches, SubCommand};
use config::Export as _;
use config::Import as _;
use config::{Committee, KeyPair, Parameters, WorkerId};
use crypto::{set_crypto_disabled, Digest, PublicKey, SignatureService};
use env_logger::Env;
use log::warn;
use network::SimpleSender;
use primary::Header;
use primary::Primary;
use primary::PrimaryWorkerMessage;
use std::collections::HashMap;
use std::net::SocketAddr;
use store::Store;
use tokio::sync::mpsc::{channel, Receiver};
use worker::Worker;

/// The default channel capacity.
pub const CHANNEL_CAPACITY: usize = 1_000;

#[tokio::main]
async fn main() -> Result<()> {
    //std::env::set_var("RUST_BACKTRACE", "1");
    
    let matches = App::new(crate_name!())
        .version(crate_version!())
        .about("A research implementation of Sailfish.")
        .args_from_usage("-v... 'Sets the level of verbosity'")
        .subcommand(
            SubCommand::with_name("generate_keys")
                .about("Print a fresh key pair to file")
                .args_from_usage("--filename=<FILE> 'The file where to print the new key pair'"),
        )
        .subcommand(
            SubCommand::with_name("run")
                .about("Run a node")
                .args_from_usage("--keys=<FILE> 'The file containing the node keys'")
                .args_from_usage("--committee=<FILE> 'The file containing committee information'")
                .args_from_usage("--parameters=[FILE] 'The file containing the node parameters'")
                .args_from_usage("--store=<PATH> 'The path where to create the data store'")
                .subcommand(SubCommand::with_name("primary").about("Run a single primary"))
                .subcommand(
                    SubCommand::with_name("worker")
                        .about("Run a single worker")
                        .args_from_usage("--id=<INT> 'The worker id'"),
                )
                .setting(AppSettings::SubcommandRequiredElseHelp),
        )
        .setting(AppSettings::SubcommandRequiredElseHelp)
        .get_matches();

    let log_level = match matches.occurrences_of("v") {
        0 => "error",
        1 => "warn",
        2 => "info",
        3 => "debug",
        _ => "trace",
    };
    let mut logger = env_logger::Builder::from_env(Env::default().default_filter_or(log_level));
    #[cfg(feature = "benchmark")]
    logger.format_timestamp_millis();
    logger.init();

    match matches.subcommand() {
        ("generate_keys", Some(sub_matches)) => KeyPair::new()
            .export(sub_matches.value_of("filename").unwrap())
            .context("Failed to generate key pair")?,
        ("run", Some(sub_matches)) => run(sub_matches).await?,
        _ => unreachable!(),
    }
    Ok(())
}

// Runs either a worker or a primary.
async fn run(matches: &ArgMatches<'_>) -> Result<()> {
    let key_file = matches.value_of("keys").unwrap();
    let committee_file = matches.value_of("committee").unwrap();
    let parameters_file = matches.value_of("parameters");
    let store_path = matches.value_of("store").unwrap();

    // Read the committee and node's keypair from file.
    let keypair = KeyPair::import(key_file).context("Failed to load the node's keypair")?;
    let name = keypair.name;
    let committee =
        Committee::import(committee_file).context("Failed to load the committee information")?;

    // Load default parameters if none are specified.
    let parameters = match parameters_file {
        Some(filename) => {
            Parameters::import(filename).context("Failed to load the node's parameters")?
        }
        None => Parameters::default(),
    };

    // Wire the no-crypto toggle into the crypto module before any signing
    // service spins up. Has to happen before SignatureService::new because
    // its background task captures the current value via Signature::new on
    // each request.
    set_crypto_disabled(parameters.disable_crypto);

    // The `SignatureService` provides signatures on input digests.
    let signature_service = SignatureService::new(keypair.secret);

    // Make the data store.
    let store = Store::new(store_path).context("Failed to create a store")?;

    // Channels the sequence of certificates.
    let (tx_output, rx_output) = channel(CHANNEL_CAPACITY);

    // Channel for sending headers between DAG and Consensus
    let (tx_sailfish, rx_sailfish) = channel(CHANNEL_CAPACITY);

    // Channel for sending loopback headerds that completed validation between DAG and Consensus
    //let (tx_validation, rx_validation) = channel(CHANNEL_CAPACITY);

    // Channel for indicating commit and that new header should be proposed
    //let (tx_ticket, rx_ticket) = channel(CHANNEL_CAPACITY);
    // Channel for sending whether async to the worker
    let (tx_async, rx_async) = channel(CHANNEL_CAPACITY);

    // Check whether to run a primary, a worker, or an entire authority.
    //Note: Each node has at most one worker. Workers that don't include a primary (e.g. are not an entire authority) use PrimaryConnector to connect to a designated primary.
    match matches.subcommand() {
        // Spawn the primary and consensus core.
        ("primary", _) => {
            let (tx_new_certificates, rx_new_certificates) = channel(CHANNEL_CAPACITY);
            let (tx_feedback, rx_feedback) = channel(CHANNEL_CAPACITY);
            let (tx_committer, rx_committer) = channel(CHANNEL_CAPACITY);
            let (tx_pushdown_cert, rx_pushdown_cert) = channel(CHANNEL_CAPACITY);
            let(tx_request_header_sync, rx_request_header_sync) = channel(CHANNEL_CAPACITY);

            Primary::spawn(
                name,
                committee.clone(),
                parameters.clone(),
                signature_service.clone(),
                store.clone(),
                /* tx_consensus */ tx_new_certificates,
                tx_committer,
                rx_committer,
                /* rx_consensus */ rx_feedback,
                tx_sailfish,
                //rx_ticket,
                rx_pushdown_cert,
                rx_request_header_sync,
                tx_output,
                tx_async,
            );
            /*Consensus::spawn(
                name,
                committee,
                parameters,
                signature_service,
                store,
                /* rx_consensus */ rx_new_certificates,
                rx_committer,
                /* tx_mempool */ tx_feedback,
                tx_output,
                tx_ticket,
                tx_validation,
                rx_sailfish,
                tx_pushdown_cert,
                tx_request_header_sync,
            );*/
        }

        // Spawn a single worker.
        ("worker", Some(sub_matches)) => {
            let id = sub_matches
                .value_of("id")
                .unwrap()
                .parse::<WorkerId>()
                .context("The worker id must be a positive integer")?;
            Worker::spawn(keypair.name, id, committee.clone(), parameters.clone(), store);
        }
        _ => unreachable!(),
    }

    // Analyze the consensus' output.
    analyze(rx_output, name, committee, parameters.client_reply_count).await;

    // If this expression is reached, the program ends and all other tasks terminate.
    unreachable!();
}

/// Receives an ordered list of committed headers and applies any
/// application-specific logic.
///
/// The application logic here is the client reply path. A committed header
/// names its batches by digest only; the transactions themselves sit in the
/// worker's store, in a different process. So this tells our own workers
/// which batches committed and leaves them to answer the clients.
///
/// Only the primary ever receives on this channel — the worker subcommand
/// leaves it idle — so this loop is the primary's commit hook.
async fn analyze(
    mut rx_output: Receiver<Header>,
    name: PublicKey,
    committee: Committee,
    client_reply_count: usize,
) {
    if client_reply_count == 0 {
        // Published behaviour: clients are send-only, so there is nothing to
        // tell the workers. Drain the channel so the committer never blocks.
        while rx_output.recv().await.is_some() {}
        return;
    }

    let mut network = SimpleSender::new();
    let mut worker_addresses: HashMap<WorkerId, Option<SocketAddr>> = HashMap::new();

    while let Some(header) = rx_output.recv().await {
        // Group the header's batches by the worker of ours that stores them.
        let mut by_worker: HashMap<WorkerId, Vec<Digest>> = HashMap::new();
        for (digest, worker_id) in &header.payload {
            by_worker
                .entry(*worker_id)
                .or_insert_with(Vec::new)
                .push(digest.clone());
        }

        for (worker_id, digests) in by_worker {
            let address = *worker_addresses.entry(worker_id).or_insert_with(|| {
                match committee.worker(&name, &worker_id) {
                    Ok(addresses) => Some(addresses.primary_to_worker),
                    Err(e) => {
                        warn!("Cannot notify worker {} of commits: {}", worker_id, e);
                        None
                    }
                }
            });
            let address = match address {
                Some(address) => address,
                None => continue,
            };

            let message = PrimaryWorkerMessage::Committed(digests, header.author);
            let bytes = bincode::serialize(&message)
                .expect("Failed to serialize our own commit notification");
            network.send(address, Bytes::from(bytes)).await;
        }
    }
}

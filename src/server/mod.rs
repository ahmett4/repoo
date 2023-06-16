use crate::{
    block::{
        parser::BlockParser, precomputed::PrecomputedBlock, receiver::BlockReceiver,
        store::BlockStoreConn, BlockHash,
    },
    state::{
        ledger::{self, genesis::GenesisRoot, public_key::PublicKey, Ledger},
        summary::{DbStats, Summary},
        IndexerState,
    },
    MAINNET_GENESIS_HASH, MAINNET_TRANSITION_FRONTIER_K, SOCKET_NAME,
};
use clap::Parser;
use futures::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use interprocess::local_socket::tokio::{LocalSocketListener, LocalSocketStream};
use std::{path::PathBuf, process, str::FromStr};
use time::PrimitiveDateTime;
use tokio::{
    fs::{self, create_dir_all, metadata},
    time::Instant,
};
use tracing::{debug, error, info, instrument, level_filters::LevelFilter};
use tracing_subscriber::prelude::*;
use uuid::Uuid;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct ServerArgs {
    /// Path to genesis ledger
    #[arg(short, long)]
    genesis_ledger: PathBuf,
    /// Hash of startup ledger
    #[arg(
        long,
        default_value = MAINNET_GENESIS_HASH
    )]
    root_hash: String,
    /// Path to startup blocks directory
    #[arg(short, long, default_value = concat!(env!("HOME"), "/mina-indexer/startup-blocks"))]
    startup_dir: PathBuf,
    /// Path to directory to watch for new blocks
    #[arg(short, long, default_value = concat!(env!("HOME"), "/mina-indexer/watch-blocks"))]
    watch_dir: PathBuf,
    /// Path to directory for rocksdb
    #[arg(short, long, default_value = concat!(env!("HOME"), "/mina-indexer/database"))]
    database_dir: PathBuf,
    /// Path to directory for logs
    #[arg(short, long, default_value = concat!(env!("HOME"), "/mina-indexer/logs"))]
    log_dir: PathBuf,
    /// Max stdout log level
    #[arg(long, default_value_t = LevelFilter::INFO)]
    log_level: LevelFilter,
    /// Override an existing db on the path provided by database_dir
    #[arg(long, default_value_t = false)]
    db_override: bool,
    /// Interval for pruning the root branch
    #[arg(short, long)]
    prune_interval: Option<u32>,
}

pub struct IndexerConfiguration {
    genesis_ledger: GenesisRoot,
    root_hash: BlockHash,
    startup_dir: PathBuf,
    watch_dir: PathBuf,
    database_dir: PathBuf,
    log_file: PathBuf,
    log_level: LevelFilter,
    prune_interval: Option<u32>,
}

#[instrument]
pub async fn handle_command_line_arguments(
    args: ServerArgs,
) -> anyhow::Result<IndexerConfiguration> {
    debug!("Parsing server args");
    let root_hash = BlockHash(args.root_hash.to_string());
    let startup_dir = args.startup_dir;
    let watch_dir = args.watch_dir;
    let database_dir = args.database_dir;
    let log_dir = args.log_dir;
    let log_level = args.log_level;
    let prune_interval = args.prune_interval;

    create_dir_if_non_existent(watch_dir.to_str().unwrap()).await;
    create_dir_if_non_existent(log_dir.to_str().unwrap()).await;

    info!(
        "Parsing genesis ledger file at {}",
        args.genesis_ledger.display()
    );

    match ledger::genesis::parse_file(&args.genesis_ledger).await {
        Err(err) => {
            error!(
                reason = "Unable to parse genesis ledger",
                error = err.to_string(),
                path = &args.genesis_ledger.display().to_string()
            );
            process::exit(100)
        }
        Ok(genesis_ledger) => {
            info!("Genesis ledger parsed successfully!");

            let mut log_number = 0;
            let mut log_fname = format!("{}/mina-indexer-0.log", log_dir.display());

            while tokio::fs::metadata(&log_fname).await.is_ok() {
                log_number += 1;
                log_fname = format!("{}/mina-indexer-log-{}", log_dir.display(), log_number);
            }

            Ok(IndexerConfiguration {
                genesis_ledger,
                root_hash,
                startup_dir,
                watch_dir,
                database_dir,
                log_file: PathBuf::from(&log_fname),
                log_level,
                prune_interval,
            })
        }
    }
}

#[instrument]
pub async fn run(args: ServerArgs) -> Result<(), anyhow::Error> {
    debug!("Checking that a server instance isn't already running");
    LocalSocketStream::connect(SOCKET_NAME)
        .await
        .expect_err("Server is already running... Exiting.");

    info!("Starting mina-indexer server");
    let IndexerConfiguration {
        genesis_ledger,
        root_hash,
        startup_dir,
        watch_dir,
        database_dir,
        log_file,
        log_level,
        prune_interval,
    } = handle_command_line_arguments(args).await?;

    // setup tracing
    if let Some(parent) = log_file.parent() {
        create_dir_if_non_existent(parent.to_str().unwrap()).await;
    }

    let log_file = std::fs::File::create(log_file)?;
    let file_layer = tracing_subscriber::fmt::layer().with_writer(log_file);

    let stdout_layer = tracing_subscriber::fmt::layer();
    tracing_subscriber::registry()
        .with(stdout_layer.with_filter(log_level))
        .with(file_layer.with_filter(LevelFilter::DEBUG))
        .init();

    // TODO
    // if !db_override
    // check if db has blocks and reconstitute state before reading blocks from startup_dir
    info!("Initializing indexer state");
    let mut indexer_state = IndexerState::new(
        root_hash.clone(),
        genesis_ledger.ledger,
        Some(&database_dir),
        Some(MAINNET_TRANSITION_FRONTIER_K),
        prune_interval,
    )?;

    let init_dir = startup_dir.display().to_string();
    info!("Ingesting precomputed blocks from {init_dir}");

    let mut block_parser = BlockParser::new(&startup_dir)?;
    let mut block_count = 0;
    let ingestion_time = Instant::now();

    while let Some(block) = block_parser.next().await? {
        debug!(
            "Adding {:?} with length {:?} to the state",
            &block.state_hash, &block.blockchain_length
        );
        indexer_state.add_block(&block)?;
        block_count += 1;
    }

    info!(
        "Ingested {block_count} blocks in {:?}",
        ingestion_time.elapsed()
    );

    let mut block_receiver = BlockReceiver::new().await?;
    info!("Block receiver set to watch {watch_dir:?}");
    block_receiver.load_directory(&watch_dir).await?;

    let listener = LocalSocketListener::bind(SOCKET_NAME)?;
    info!("Local socket listener started");

    loop {
        tokio::select! {
            block_fut = block_receiver.recv() => {
                if let Some(block_result) = block_fut {
                    let precomputed_block = block_result?;
                    debug!("Receiving block {:?}", precomputed_block);

                    indexer_state.add_block(&precomputed_block)?;

                    info!("Added block with height: {}, state_hash: {:?}", &precomputed_block.state_hash, precomputed_block.blockchain_length.unwrap_or(0));
                } else {
                    info!("Block receiver shutdown, system exit");
                    return Ok(())
                }
            }

            conn_fut = listener.accept() => {
                let conn = conn_fut?;
                info!("Receiving connection");
                let best_chain = indexer_state.root_branch.longest_chain();

                let primary_path = database_dir.clone();
                let mut secondary_path = primary_path.clone();
                secondary_path.push(Uuid::new_v4().to_string());

                debug!("Spawning secondary readonly RocksDB instance");
                let block_store_readonly = BlockStoreConn::new_read_only(&primary_path, &secondary_path)?;

                // state summary
                let mut max_dangling_height = 0;
                let mut max_dangling_length = 0;

                for dangling in &indexer_state.dangling_branches {
                    if dangling.height() > max_dangling_height {
                        max_dangling_height = dangling.height();
                    }
                    if dangling.len() > max_dangling_length {
                        max_dangling_length = dangling.len();
                    }
                }

                let db_stats_str = indexer_state
                    .block_store
                    .as_ref()
                    .map(|db| db.db_stats());
                let mem = indexer_state
                    .block_store
                    .as_ref()
                    .map(|db| db.memtables_size())
                    .unwrap_or_default();
                let summary = Summary {
                    uptime: indexer_state.time.clone().elapsed(),
                    date_time: PrimitiveDateTime::new(indexer_state.date_time.date(), indexer_state.date_time.time()),
                    blocks_processed: indexer_state.blocks_processed,
                    best_tip_hash: indexer_state.best_tip.state_hash.0.clone(),
                    root_hash: indexer_state.root_branch.root.state_hash.0.clone(),
                    root_height: indexer_state.root_branch.height(),
                    root_length: indexer_state.root_branch.len(),
                    num_leaves: indexer_state.root_branch.leaves().len(),
                    num_dangling: indexer_state.dangling_branches.len(),
                    max_dangling_height,
                    max_dangling_length,
                    db_stats: db_stats_str.map(|s| DbStats::from_str(&format!("{mem}\n{s}")).unwrap()),
                };
                let ledger = indexer_state.root_branch.best_tip().unwrap().get_ledger().clone();

                // handle the connection
                tokio::spawn(async move {
                    debug!("Handling connection");
                    if let Err(e) = handle_conn(conn, block_store_readonly, best_chain, ledger, summary).await {
                        error!("Error handling connection: {e}");
                    }

                    debug!("Removing readonly instance at {}", secondary_path.display());
                    tokio::fs::remove_dir_all(&secondary_path).await.ok();
                });
            }
        }
    }
}

#[instrument]
async fn handle_conn(
    conn: LocalSocketStream,
    db: BlockStoreConn,
    best_chain: Vec<BlockHash>,
    ledger: Ledger,
    summary: Summary,
) -> Result<(), anyhow::Error> {
    let (reader, mut writer) = conn.into_split();
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::with_capacity(128);
    let _read = reader.read_until(0, &mut buffer).await?;

    let mut buffers = buffer.split(|byte| *byte == 32);
    let command = buffers.next().unwrap();

    let command_string = String::from_utf8(command.to_vec())?;

    match command_string.as_str() {
        "account" => {
            let data_buffer = buffers.next().unwrap();
            let public_key = PublicKey::from_address(&String::from_utf8(
                data_buffer[..data_buffer.len() - 1].to_vec(),
            )?)?;
            info!("Received account command for {public_key:?}");
            debug!("Using ledger {ledger:?}");
            let account = ledger.accounts.get(&public_key);
            if let Some(account) = account {
                debug!("Writing account {account:?} to client");
                let bytes = bcs::to_bytes(account)?;
                writer.write_all(&bytes).await?;
            }
        }
        "best_chain" => {
            info!("Received best_chain command");
            let data_buffer = buffers.next().unwrap();
            let num = String::from_utf8(data_buffer[..data_buffer.len() - 1].to_vec())?
                .parse::<usize>()?;
            let best_chain: Vec<PrecomputedBlock> = best_chain[..best_chain.len() - 1]
                .iter()
                .take(num)
                .cloned()
                .map(|state_hash| db.get_block(&state_hash.0).unwrap().unwrap())
                .collect();
            let bytes = bcs::to_bytes(&best_chain)?;
            writer.write_all(&bytes).await?;
        }
        "best_ledger" => {
            info!("Received best_ledger command");
            let data_buffer = buffers.next().unwrap();
            let path = &String::from_utf8(data_buffer[..data_buffer.len() - 1].to_vec())?
                .parse::<PathBuf>()?;
            debug!("Writing ledger to {}", path.display());
            fs::write(path, format!("{ledger:?}")).await?;
            let bytes = bcs::to_bytes(&format!("Ledger written to {}", path.display()))?;
            writer.write_all(&bytes).await?;
        }
        "summary\0" => {
            info!("Received summary command");
            let bytes = bcs::to_bytes(&summary)?;
            writer.write_all(&bytes).await?;
        }
        bad_request => {
            let err_msg = format!("Malformed request: {bad_request}");
            error!("{err_msg}");
            return Err(anyhow::Error::msg(err_msg));
        }
    }

    Ok(())
}

async fn create_dir_if_non_existent(path: &str) {
    if metadata(path).await.is_err() {
        create_dir_all(path).await.unwrap();
    }
}

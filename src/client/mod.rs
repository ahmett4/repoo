use crate::{
    block::{precomputed::PrecomputedBlock, Block},
    state::{
        ledger::account::Account,
        summary::{SummaryShort, SummaryVerbose},
    },
    SOCKET_NAME,
};
use clap::Parser;
use futures::{
    io::{AsyncWriteExt, BufReader},
    AsyncReadExt,
};
use interprocess::local_socket::tokio::LocalSocketStream;
use std::{path::PathBuf, process};
use tracing::instrument;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub enum ClientCli {
    /// Display the account info for the given public key
    Account(AccountArgs),
    /// Display the best chain
    BestChain(ChainArgs),
    /// Dump the best ledger to a file
    BestLedger(LedgerArgs),
    /// Show summary of indexer state
    Summary(SummaryArgs),
}

#[derive(clap::Args, Debug)]
#[command(author, version, about, long_about = None)]
pub struct AccountArgs {
    /// Retrieve this public key's account info
    #[arg(short, long)]
    public_key: String,
}

#[derive(clap::Args, Debug)]
#[command(author, version, about, long_about = None)]
pub struct ChainArgs {
    /// Number of blocks to include
    #[arg(short, long)]
    num: usize,
    /// Path to write the best chain (default: stdout)
    #[arg(short, long)]
    path: Option<PathBuf>,
    /// Verbose displays the entire precomputed block (default: false)
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
}

#[derive(clap::Args, Debug)]
#[command(author, version, about, long_about = None)]
pub struct LedgerArgs {
    /// Path to write the ledger
    #[arg(short, long)]
    path: PathBuf,
}

#[derive(clap::Args, Debug)]
#[command(author, version, about, long_about = None)]
pub struct SummaryArgs {
    /// Verbose output should be redirected to a file
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
}

#[instrument]
pub async fn run(command: &ClientCli) -> Result<(), anyhow::Error> {
    let conn = match LocalSocketStream::connect(SOCKET_NAME).await {
        Ok(conn) => conn,
        Err(e) => {
            println!(
                "Make sure the server has been started and initial block ingestion has completed."
            );
            println!(
                "Initial block ingestion takes several minutes if ingesting all mainnet blocks."
            );
            println!("Error: {e}");
            process::exit(111);
        }
    };
    let (reader, mut writer) = conn.into_split();
    let mut reader = BufReader::new(reader);

    let mut buffer = Vec::with_capacity(1280000);

    match command {
        ClientCli::Account(account_args) => {
            let command = format!("account {}\0", account_args.public_key);
            writer.write_all(command.as_bytes()).await?;
            reader.read_to_end(&mut buffer).await?;
            let account: Account = bcs::from_bytes(&buffer)?;
            println!("{account:?}");
        }
        ClientCli::BestChain(chain_args) => {
            let command = format!("best_chain {}\0", chain_args.num);
            writer.write_all(command.as_bytes()).await?;
            reader.read_to_end(&mut buffer).await?;
            let blocks: Vec<PrecomputedBlock> = bcs::from_bytes(&buffer)?;
            blocks.iter().for_each(|block| {
                if chain_args.verbose {
                    println!("{}", serde_json::to_string(block).unwrap());
                } else {
                    let block = Block::from_precomputed(block, block.blockchain_length.unwrap());
                    println!("{}", block.summary());
                }
            });
        }
        ClientCli::BestLedger(ledger_args) => {
            let command = format!("best_ledger {}\0", ledger_args.path.display());
            writer.write_all(command.as_bytes()).await?;
            reader.read_to_end(&mut buffer).await?;
            let msg: String = bcs::from_bytes(&buffer)?;
            println!("{msg}");
        }
        ClientCli::Summary(summary_args) => {
            let command = format!("summary {}\0", summary_args.verbose);
            writer.write_all(command.as_bytes()).await?;
            reader.read_to_end(&mut buffer).await?;
            if summary_args.verbose {
                let summary: SummaryVerbose = bcs::from_bytes(&buffer)?;
                println!("{summary}");
            } else {
                let summary: SummaryShort = bcs::from_bytes(&buffer)?;
                println!("{summary}");
            }
        }
    }

    Ok(())
}

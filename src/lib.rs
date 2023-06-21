pub mod block;
pub mod client;
pub mod server;
pub mod state;
pub mod store;

pub const BLOCK_REPORTING_FREQ: u32 = 5000;
pub const MAINNET_CANONICAL_THRESHOLD: u32 = 10;
pub const MAINNET_GENESIS_HASH: &str = "3NKeMoncuHab5ScarV5ViyF16cJPT4taWNSaTLS64Dp67wuXigPZ";
pub const MAINNET_TRANSITION_FRONTIER_K: u32 = 290;
pub const PRUNE_INTERVAL_DEFAULT: u32 = 10;
pub const SOCKET_NAME: &str = "@mina-indexer.sock";

pub const ROCKSDB_TUNING_CONFIG_FILE: &str = "tuning.config";
pub const ROCKSDB_WRITE_BUFFER_SIZE: usize = 512 * 1024 * 1024; // 512 MB
pub const ROCKSDB_TARGET_FILE_SIZE: u64 = 1024 * 1024 * 1024; // 1 GB

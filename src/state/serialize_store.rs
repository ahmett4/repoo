use std::{io::{BufReader, Read}, path::PathBuf};

use rocksdb::backup::{BackupEngineOptions, BackupEngine, RestoreOptions};
use serde::{Serializer, ser, Deserializer, de::{Visitor, SeqAccess}};
use tar::Archive;
use tracing::{instrument, trace};

use crate::{store::IndexerStore, AMAZON_ATHENA_DEFAULT_ZSTD_COMPRESSION_LEVEL};

#[instrument(skip(store, serializer))]
pub fn serialize<S>(store: &Option<IndexerStore>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match store {
        None => {
            trace!("serializing None IndexerStore");
            serializer.serialize_none()
        },
        Some(indexer_store) => {
            trace!("serializing IndexerStore");
            let backup_tarball = (move || {
                trace!("Initializing RocksDB BackupEngine");
                let backup_opts = BackupEngineOptions::new("./rocksdb_backup")?;
                let backup_env = rocksdb::Env::new()?;
                let mut backup_engine = BackupEngine::open(&backup_opts, &backup_env)?;
                trace!("Flushing database operations to disk and Creating new RocksDB Backup");
                backup_engine.create_new_backup_flush(indexer_store.db(), true)?;
                trace!("Creating temporary output file at ./rocksdb_backup.tar.zst");
                let tarball_file = std::fs::File::create("./rocksdb_backup.tar.zst")?;
                trace!("Initializing zstd encoder for {:?}", tarball_file);
                let encoder =
                    zstd::Encoder::new(tarball_file, AMAZON_ATHENA_DEFAULT_ZSTD_COMPRESSION_LEVEL)?;
                trace!("Creating new tar archive builder");
                let mut tar = tar::Builder::new(encoder);
                trace!("Adding the RocksDB backup to the archive");
                tar.append_dir("rocksdb_backup", "./rocksdb_backup")?;
                drop(tar.into_inner()?.finish()?);
                let mut tarball_file = std::fs::File::open("./rocksdb_backup.tar.zst")?;
                trace!("Finalizing tarball file {:?}", tarball_file);
                trace!("Reading compressed tarball to byte array");
                let mut tarball_bytes = Vec::new();
                tarball_file.read_to_end(&mut tarball_bytes)?;
                trace!("Read {} bytes into byte array", tarball_bytes.len());
                if std::fs::metadata("./rocksdb").is_ok() {
                    trace!("removing rocksdb backup directory");
                    std::fs::remove_dir_all("./rocksdb")?;
                }
                if std::fs::metadata("./rocksdb_backup.tar.zst").is_ok() {
                    trace!("removing temporary tarball file");
                    std::fs::remove_file("./rocksdb_backup.tar.zst")?
                }
                Ok(tarball_bytes)
            })()
            .map_err(|e: anyhow::Error| ser::Error::custom(e.to_string()))?;
            trace!("serializing compressed backup tarball as Some");
            serializer.serialize_some(&backup_tarball)
        }
    }
}

#[instrument(skip(deserializer))]
pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<IndexerStore>, D::Error>
where
    D: Deserializer<'de>,
{
    struct IndexerStoreVisitor;

    impl<'de> Visitor<'de> for IndexerStoreVisitor {
        type Value = Option<IndexerStore>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("Option value")
        }

        fn visit_seq<V>(self, mut seq: V) -> Result<Option<IndexerStore>, V::Error>
        where
            V: SeqAccess<'de>,
        {
            trace!("deserializing bytes from Option");
            let option_bytes: Option<Vec<u8>> = seq
                .next_element()?
                .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;

            trace!("got Option<Vec<u8>>, mapping database restore operation over Some value");
            option_bytes
                .map(|bytes| {
                    let reader = BufReader::new(bytes.as_slice());
                    let decoder = zstd::Decoder::new(reader)?;
                    let mut archive = Archive::new(decoder);
                    trace!("unpacking backup data into ./rocksdb_backup");
                    archive.unpack("./rocksdb_backup")?;
                    trace!("initializing backup engine");
                    let backup_opts = BackupEngineOptions::new("./rocksdb_backup")?;
                    let backup_env = rocksdb::Env::new()?;
                    let mut backup_engine = BackupEngine::open(&backup_opts, &backup_env)?;
                    trace!("restoring backup to ./rocksdb");
                    backup_engine.restore_from_latest_backup(
                        "./rocksdb",
                        "./rocksdb",
                        &RestoreOptions::default(),
                    )?;
                    trace!("initializing IndexerStore with restored database instance");
                    IndexerStore::new(&PathBuf::from("./rocksdb"))
                })
                .map(|result| {
                    result.map_err(|e: anyhow::Error| serde::de::Error::custom(e.to_string()))
                })
                .map_or(Ok(None), |v| v.map(Some))
        }
    }
    deserializer.deserialize_option(IndexerStoreVisitor)
}
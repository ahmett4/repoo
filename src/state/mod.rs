use crate::{
    block::{
        parser::BlockParser,
        precomputed::PrecomputedBlock,
        store::{BlockStore, CanonicityStore},
        Block, BlockHash,
    },
    state::{
        branch::Branch,
        ledger::{
            command::Command, diff::LedgerDiff, genesis::GenesisLedger, store::LedgerStore, Ledger,
        },
    },
    store::IndexerStore,
    BLOCK_REPORTING_FREQ, LEDGER_UPDATE_FREQ, MAINNET_CANONICAL_THRESHOLD, PRUNE_INTERVAL_DEFAULT,
};
use id_tree::NodeId;
use serde_derive::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path, time::Instant};
use time::OffsetDateTime;
use tracing::{debug, info};

pub mod branch;
pub mod ledger;
pub mod summary;

/// Rooted forest of precomputed block summaries aka the witness tree
/// `root_branch` - represents the tree of blocks connecting back to a known ledger state, e.g. genesis
/// `dangling_branches` - trees of blocks stemming from an unknown ledger state
pub struct IndexerState {
    /// Indexer mode
    pub mode: IndexerMode,
    /// Indexer phase
    pub phase: IndexerPhase,
    /// Block representing the best tip of the root branch
    pub best_tip: NodeId,
    /// Highest known canonical block
    pub canonical_tip: NodeId,
    /// Map of ledger diffs following the canonical tip
    pub diffs_map: HashMap<BlockHash, LedgerDiff>,
    /// Append-only tree of blocks built from genesis, each containing a ledger
    pub root_branch: Branch,
    /// Dynamic, dangling branches eventually merged into the `root_branch`
    /// needed for the possibility of missing blocks
    pub dangling_branches: Vec<Branch>,
    /// Block database
    pub indexer_store: Option<IndexerStore>,
    /// Threshold amount of confirmations to trigger a pruning event
    pub transition_frontier_length: Option<u32>,
    /// Interval to the prune the root branch
    pub prune_interval: Option<u32>,
    /// How often to update the canonical ledger
    pub ledger_update_freq: u32,
    /// Number of blocks added to the state
    pub blocks_processed: u32,
    /// Time the indexer started running
    pub time: Instant,
    /// Datetime the indexer started running
    pub date_time: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub enum IndexerPhase {
    InitializingFromBlockDir,
    InitializingFromDB,
    Watching,
    Testing,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum IndexerMode {
    Light,
    Full,
    Test,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExtensionType {
    DanglingNew,
    DanglingSimpleForward,
    DanglingSimpleReverse,
    DanglingComplex,
    RootSimple,
    RootComplex,
    BlockNotAdded,
}

pub enum ExtensionDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum Canonicity {
    Canonical,
    Orphaned,
    Pending,
}

impl IndexerState {
    pub fn new(
        mode: IndexerMode,
        root_hash: BlockHash,
        genesis_ledger: GenesisLedger,
        rocksdb_path: Option<&Path>,
        transition_frontier_length: Option<u32>,
        prune_interval: Option<u32>,
    ) -> anyhow::Result<Self> {
        let root_branch = Branch::new_genesis(root_hash.clone());
        let indexer_store = rocksdb_path.map(|path| {
            let store = IndexerStore::new(path).unwrap();
            store
                .add_ledger(&root_hash, genesis_ledger.into())
                .expect("ledger add succeeds");
            store
        });
        Ok(Self {
            mode,
            phase: IndexerPhase::InitializingFromBlockDir,
            canonical_tip: root_branch.root.clone(),
            diffs_map: HashMap::new(),
            best_tip: root_branch.root.clone(),
            root_branch,
            dangling_branches: Vec::new(),
            indexer_store,
            transition_frontier_length,
            prune_interval,
            ledger_update_freq: LEDGER_UPDATE_FREQ,
            blocks_processed: 0,
            time: Instant::now(),
            date_time: OffsetDateTime::now_utc(),
        })
    }

    /// Start a new indexer state from a canonical ledger
    pub fn new_non_genesis(
        mode: IndexerMode,
        root_hash: BlockHash,
        ledger: Ledger,
        blockchain_length: Option<u32>,
        rocksdb_path: Option<&Path>,
        transition_frontier_length: Option<u32>,
        prune_interval: Option<u32>,
    ) -> anyhow::Result<Self> {
        let root_branch = Branch::new_non_genesis(root_hash.clone(), blockchain_length);
        let indexer_store = rocksdb_path.map(|path| {
            let store = IndexerStore::new(path).unwrap();
            store
                .add_ledger(&root_hash, ledger)
                .expect("ledger add succeeds");
            store
        });
        Ok(Self {
            mode,
            phase: IndexerPhase::InitializingFromDB,
            canonical_tip: root_branch.root.clone(),
            diffs_map: HashMap::new(),
            best_tip: root_branch.root.clone(),
            root_branch,
            dangling_branches: Vec::new(),
            indexer_store,
            transition_frontier_length,
            prune_interval,
            ledger_update_freq: LEDGER_UPDATE_FREQ,
            blocks_processed: 0,
            time: Instant::now(),
            date_time: OffsetDateTime::now_utc(),
        })
    }

    pub fn new_testing(
        root_block: &PrecomputedBlock,
        root_ledger: Option<Ledger>,
        rocksdb_path: Option<&Path>,
        transition_frontier_length: Option<u32>,
    ) -> anyhow::Result<Self> {
        let root_branch = Branch::new_testing(root_block);
        let indexer_store = rocksdb_path.map(|path| {
            let store = IndexerStore::new(path).unwrap();
            if let Some(ledger) = root_ledger {
                store
                    .add_ledger(&BlockHash(root_block.state_hash.clone()), ledger)
                    .expect("ledger add succeeds");
            }
            store
        });
        Ok(Self {
            mode: IndexerMode::Test,
            phase: IndexerPhase::Testing,
            canonical_tip: root_branch.root.clone(),
            diffs_map: HashMap::new(),
            best_tip: root_branch.root.clone(),
            root_branch,
            dangling_branches: Vec::new(),
            indexer_store,
            transition_frontier_length,
            prune_interval: None,
            ledger_update_freq: LEDGER_UPDATE_FREQ,
            blocks_processed: 0,
            time: Instant::now(),
            date_time: OffsetDateTime::now_utc(),
        })
    }

    pub fn new_from_db(path: &Path) -> anyhow::Result<Self> {
        let msg = format!("Restore from {}", path.display());
        todo!("{msg}")
    }

    fn prune_root_branch(&mut self) {
        if let Some(k) = self.transition_frontier_length {
            let interval = self.prune_interval.unwrap_or(PRUNE_INTERVAL_DEFAULT);
            let best_tip_block = self.best_tip_block().clone();
            if self.root_branch.height() as u32 > interval * k {
                debug!(
                    "Pruning transition frontier at k = {}, best tip length: {}",
                    k,
                    self.best_tip_block().blockchain_length.unwrap_or(0)
                );
                self.root_branch
                    .prune_transition_frontier(k, &best_tip_block);
            }
        }
    }

    pub fn canonical_tip_block(&self) -> &Block {
        self.get_block_from_id(&self.canonical_tip)
    }

    pub fn best_tip_block(&self) -> &Block {
        self.get_block_from_id(&self.best_tip)
    }

    /// Only works with blocks in the root branch
    fn get_block_from_id(&self, node_id: &NodeId) -> &Block {
        self.root_branch.branches.get(node_id).unwrap().data()
    }

    fn update_canonical(&mut self) {
        let mut canonical_hashes = vec![];
        let old_canonical_tip_id = self.canonical_tip.clone();
        let old_canonical_tip_hash = self.canonical_tip_block().state_hash.clone();

        // update canonical_tip
        for (n, ancestor_id) in self
            .root_branch
            .branches
            .ancestor_ids(&self.best_tip)
            .unwrap()
            .enumerate()
        {
            if n <= MAINNET_CANONICAL_THRESHOLD as usize {
                let ancestor_block = self.get_block_from_id(ancestor_id);
                canonical_hashes.push(ancestor_block.state_hash.clone());
            } else {
                self.canonical_tip = ancestor_id.clone();
                break;
            }
        }

        canonical_hashes.reverse();

        // update canonical ledger
        if self.best_tip_block().height % self.ledger_update_freq == 0 {
            if let Some(indexer_store) = &self.indexer_store {
                let mut ledger = indexer_store
                    .get_ledger(&old_canonical_tip_hash)
                    .unwrap()
                    .unwrap();

                for canonical_hash in &canonical_hashes {
                    if let Some(diff) = self.diffs_map.get(canonical_hash) {
                        ledger.apply_diff(diff).unwrap();
                    }
                }

                indexer_store
                    .add_ledger(&self.canonical_tip_block().state_hash, ledger)
                    .unwrap();
            }
        }

        // update canonicity store
        for block_hash in self.diffs_map.keys() {
            if let Some(indexer_store) = &self.indexer_store {
                if canonical_hashes.contains(block_hash) {
                    indexer_store.add_canonical(block_hash).unwrap();
                } else {
                    indexer_store.add_orphaned(block_hash).unwrap();
                }
            }
        }

        // remove diffs corresponding to blocks at or beneath the height of the new canonical tip
        for node_id in self
            .root_branch
            .branches
            .traverse_level_order_ids(&old_canonical_tip_id)
            .unwrap()
        {
            if self.get_block_from_id(&node_id).height <= self.canonical_tip_block().height {
                self.diffs_map
                    .remove(&self.get_block_from_id(&node_id).state_hash.clone());
            }
        }
    }

    /// Adds blocks to the state according to block_parser then changes phase to Watching
    ///
    /// Returns the number of blocks parsed
    pub async fn add_blocks(&mut self, block_parser: &mut BlockParser) -> anyhow::Result<u32> {
        let mut block_count = 0;
        let time = Instant::now();

        while let Some(block) = block_parser.next().await? {
            if block_count > 0 && block_count % BLOCK_REPORTING_FREQ == 0 {
                info!(
                    "{}",
                    format!(
                        "Parsed and added {block_count} blocks to the witness tree in {:?}",
                        time.elapsed()
                    )
                );
            }
            self.add_block(&block)?;
            block_count += 1;
        }

        debug!("Phase change: {} -> {}", self.phase, IndexerPhase::Watching);
        self.phase = IndexerPhase::Watching;
        Ok(block_count)
    }

    /// Adds the block to the witness tree and the precomputed block to the db
    ///
    /// Errors if the block is already present in the witness tree
    pub fn add_block(
        &mut self,
        precomputed_block: &PrecomputedBlock,
    ) -> anyhow::Result<ExtensionType> {
        self.prune_root_branch();

        if self.is_block_already_in_db(precomputed_block)? {
            debug!(
                "Block with state hash {:?} is already present in the block store",
                precomputed_block.state_hash
            );
            return Ok(ExtensionType::BlockNotAdded);
        }

        if let Some(indexer_store) = self.indexer_store.as_ref() {
            indexer_store.add_block(precomputed_block)?;
        }
        self.blocks_processed += 1;

        // forward extension on root branch
        if self.is_length_within_root_bounds(precomputed_block) {
            if let Some(root_extension) = self.root_extension(precomputed_block)? {
                return Ok(root_extension);
            }
        }

        // if a dangling branch has been extended (forward or reverse) check for new connections to other dangling branches
        if let Some((extended_branch_index, new_node_id, direction)) =
            self.dangling_extension(precomputed_block)?
        {
            return self.update_dangling(
                precomputed_block,
                extended_branch_index,
                new_node_id,
                direction,
            );
        }

        self.diffs_map.insert(
            BlockHash(precomputed_block.state_hash.clone()),
            LedgerDiff::from_precomputed_block(precomputed_block),
        );

        self.new_dangling(precomputed_block)
    }

    fn root_extension(
        &mut self,
        precomputed_block: &PrecomputedBlock,
    ) -> anyhow::Result<Option<ExtensionType>> {
        if let Some(new_node_id) = self.root_branch.simple_extension(precomputed_block) {
            self.update_best_tip();
            self.update_canonical();

            // check if new block connects to a dangling branch
            let mut branches_to_remove = Vec::new();
            for (index, dangling_branch) in self.dangling_branches.iter_mut().enumerate() {
                // new block is the parent of the dangling branch root
                if is_reverse_extension(dangling_branch, precomputed_block) {
                    self.root_branch.merge_on(&new_node_id, dangling_branch);
                    branches_to_remove.push(index);
                }

                same_block_added_twice(dangling_branch, precomputed_block)?;
            }

            if !branches_to_remove.is_empty() {
                // the root branch is newly connected to dangling branches
                for (num_removed, index_to_remove) in branches_to_remove.iter().enumerate() {
                    self.dangling_branches.remove(index_to_remove - num_removed);
                }

                self.update_best_tip();
                self.update_canonical();

                Ok(Some(ExtensionType::RootComplex))
            } else {
                // there aren't any branches that are connected
                Ok(Some(ExtensionType::RootSimple))
            }
        } else {
            Ok(None)
        }
    }

    fn dangling_extension(
        &mut self,
        precomputed_block: &PrecomputedBlock,
    ) -> anyhow::Result<Option<(usize, NodeId, ExtensionDirection)>> {
        let mut extension = None;
        for (index, dangling_branch) in self.dangling_branches.iter_mut().enumerate() {
            let min_length = dangling_branch.root_block().blockchain_length.unwrap_or(0);
            let max_length = dangling_branch
                .best_tip()
                .unwrap()
                .blockchain_length
                .unwrap_or(0);

            // check incoming block is within the length bounds
            if let Some(length) = precomputed_block.blockchain_length {
                if max_length + 1 >= length && length + 1 >= min_length {
                    // simple reverse
                    if is_reverse_extension(dangling_branch, precomputed_block) {
                        dangling_branch.new_root(precomputed_block);
                        extension = Some((
                            index,
                            dangling_branch
                                .branches
                                .root_node_id()
                                .expect("has root")
                                .clone(),
                            ExtensionDirection::Reverse,
                        ));
                        break;
                    }

                    // simple forward
                    if let Some(new_node_id) = dangling_branch.simple_extension(precomputed_block) {
                        extension = Some((index, new_node_id, ExtensionDirection::Forward));
                        break;
                    }

                    same_block_added_twice(dangling_branch, precomputed_block)?;
                }
            } else {
                // we don't know the blockchain_length for the incoming block, so we can't discriminate

                // simple reverse
                if is_reverse_extension(dangling_branch, precomputed_block) {
                    dangling_branch.new_root(precomputed_block);
                    extension = Some((
                        index,
                        dangling_branch
                            .branches
                            .root_node_id()
                            .expect("has root")
                            .clone(),
                        ExtensionDirection::Reverse,
                    ));
                    break;
                }

                // simple forward
                if let Some(new_node_id) = dangling_branch.simple_extension(precomputed_block) {
                    extension = Some((index, new_node_id, ExtensionDirection::Forward));
                    break;
                }

                same_block_added_twice(dangling_branch, precomputed_block)?;
            }
        }

        Ok(extension)
    }

    fn update_dangling(
        &mut self,
        precomputed_block: &PrecomputedBlock,
        extended_branch_index: usize,
        new_node_id: NodeId,
        direction: ExtensionDirection,
    ) -> anyhow::Result<ExtensionType> {
        let mut branches_to_update = Vec::new();
        for (index, dangling_branch) in self.dangling_branches.iter().enumerate() {
            if precomputed_block.state_hash == dangling_branch.root_block().parent_hash.0 {
                branches_to_update.push(index);
            }
        }

        if !branches_to_update.is_empty() {
            // remove one
            let mut extended_branch = self.dangling_branches.remove(extended_branch_index);
            for (n, dangling_branch_index) in branches_to_update.iter().enumerate() {
                let index = if extended_branch_index < *dangling_branch_index {
                    dangling_branch_index - n - 1
                } else {
                    *dangling_branch_index
                };
                let branch_to_update = self.dangling_branches.get_mut(index).unwrap();
                extended_branch.merge_on(&new_node_id, branch_to_update);

                // remove one for each index we see
                self.dangling_branches.remove(index);
            }

            self.dangling_branches.push(extended_branch);
            Ok(ExtensionType::DanglingComplex)
        } else {
            match direction {
                ExtensionDirection::Forward => Ok(ExtensionType::DanglingSimpleForward),
                ExtensionDirection::Reverse => Ok(ExtensionType::DanglingSimpleReverse),
            }
        }
    }

    fn new_dangling(
        &mut self,
        precomputed_block: &PrecomputedBlock,
    ) -> anyhow::Result<ExtensionType> {
        self.dangling_branches
            .push(Branch::new(precomputed_block).expect("cannot fail"));
        Ok(ExtensionType::DanglingNew)
    }

    fn is_length_within_root_bounds(&self, precomputed_block: &PrecomputedBlock) -> bool {
        (precomputed_block.blockchain_length.is_some()
            && self.best_tip_block().blockchain_length.unwrap_or(0) + 1
                >= precomputed_block.blockchain_length.unwrap())
            || precomputed_block.blockchain_length.is_none()
    }

    fn is_block_already_in_db(&self, precomputed_block: &PrecomputedBlock) -> anyhow::Result<bool> {
        if let Some(indexer_store) = self.indexer_store.as_ref() {
            match indexer_store.get_block(&BlockHash(precomputed_block.state_hash.to_string()))? {
                None => Ok(false),
                Some(_block) => Ok(true),
            }
        } else {
            Ok(false)
        }
    }

    fn update_best_tip(&mut self) {
        let (id, _) = self.root_branch.best_tip_with_id().unwrap();
        self.best_tip = id;
    }

    pub fn chain_commands(&self) -> Vec<Command> {
        if let Some(indexer_store) = self.indexer_store.as_ref() {
            return self
                .root_branch
                .longest_chain()
                .iter()
                .flat_map(|state_hash| indexer_store.get_block(state_hash))
                .flatten()
                .flat_map(|precomputed_block| Command::from_precomputed_block(&precomputed_block))
                .collect();
        }
        vec![]
    }

    pub fn get_block_status(&self, state_hash: &BlockHash) -> Option<Canonicity> {
        // check diffs map
        if self.diffs_map.get(state_hash).is_some() {
            return Some(Canonicity::Pending);
        }

        if let Some(indexer_store) = &self.indexer_store {
            return indexer_store.get_canonicity(state_hash).unwrap();
        }

        None
    }

    pub fn best_ledger(&mut self) -> anyhow::Result<Option<Ledger>> {
        self.update_canonical();

        if let Some(indexer_store) = &self.indexer_store {
            return indexer_store.get_ledger(&self.canonical_tip_block().state_hash);
        }

        Ok(None)
    }
}

fn is_reverse_extension(branch: &Branch, precomputed_block: &PrecomputedBlock) -> bool {
    precomputed_block.state_hash == branch.root_block().parent_hash.0
}

fn same_block_added_twice(
    branch: &Branch,
    precomputed_block: &PrecomputedBlock,
) -> anyhow::Result<()> {
    if precomputed_block.state_hash == branch.root_block().state_hash.0 {
        return Err(anyhow::Error::msg(
            "Same block added twice to the indexer state",
        ));
    }
    Ok(())
}

impl std::fmt::Debug for IndexerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Root branch ===")?;
        writeln!(f, "{:?}", self.root_branch)?;

        writeln!(f, "=== Dangling branches ===")?;
        for (n, branch) in self.dangling_branches.iter().enumerate() {
            writeln!(f, "Dangling branch {n}:")?;
            writeln!(f, "{branch:?}")?;
        }
        Ok(())
    }
}

impl std::fmt::Display for IndexerMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexerMode::Full => write!(f, "full"),
            IndexerMode::Light => write!(f, "light"),
            IndexerMode::Test => write!(f, "test"),
        }
    }
}

impl std::fmt::Display for IndexerPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexerPhase::InitializingFromBlockDir | IndexerPhase::InitializingFromDB => {
                write!(f, "initializing")
            }
            IndexerPhase::Watching => write!(f, "watching"),
            IndexerPhase::Testing => write!(f, "testing"),
        }
    }
}

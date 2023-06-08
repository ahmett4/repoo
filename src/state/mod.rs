use crate::block::{precomputed::PrecomputedBlock, store::BlockStoreConn, BlockHash};

use self::{
    branch::{Branch, Leaf},
    ledger::{command::Command, diff::LedgerDiff, genesis::GenesisLedger, Ledger},
};

pub mod branch;
pub mod ledger;

/// Rooted forest of precomputed block summaries
/// `root_branch` - represents the tree of blocks connecting back to a known ledger state, e.g. genesis
/// `dangling_branches` - trees of blocks stemming from an unknown ledger state
pub struct IndexerState {
    /// Longest chain of blocks from the `root_branch`
    pub best_chain: Vec<Leaf<Ledger>>,
    /// Append-only tree of blocks built from genesis, each containing a ledger
    pub root_branch: Branch<Ledger>,
    /// Dynamic, dangling branches eventually merged into the `root_branch`
    pub dangling_branches: Vec<Branch<LedgerDiff>>,
    /// Block database
    pub block_store: Option<BlockStoreConn>,
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

impl IndexerState {
    pub fn new(
        root_hash: BlockHash,
        genesis_ledger: Option<GenesisLedger>,
        blocks_path: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        let block_store = blocks_path.map(|path| BlockStoreConn::new(path).unwrap());
        Ok(Self {
            best_chain: Vec::new(),
            root_branch: Branch::new_genesis(root_hash, genesis_ledger),
            dangling_branches: Vec::new(),
            block_store,
        })
    }

    pub fn add_block(
        &mut self,
        precomputed_block: &PrecomputedBlock,
    ) -> anyhow::Result<ExtensionType> {
        self.block_store
            .as_mut()
            .map(|block_store| block_store.add_block(precomputed_block));
        // forward extension on root branch
        // TODO put heights of root and the highest leaf in Branch
        if let Some(_max_length) = self
            .root_branch
            .leaves
            .iter()
            .flat_map(|(_, x)| x.block.blockchain_length)
            .fold(None, |acc, x| acc.max(Some(x)))
        {
            // check leaf heights first
            // if max_length + 1 >= precomputed_block.blockchain_length.unwrap_or(0) {
            if let Some(new_node_id) = self.root_branch.simple_extension(precomputed_block) {
                let mut branches_to_remove = Vec::new();
                // check if new block connects to a dangling branch
                for (index, dangling_branch) in self.dangling_branches.iter_mut().enumerate() {
                    // incoming block is the parent of the dangling branch root
                    if precomputed_block.state_hash == dangling_branch.root.parent_hash.0 {
                        self.root_branch.merge_on(&new_node_id, dangling_branch);
                        branches_to_remove.push(index);
                    }

                    // if the block is already in the dangling branch, do nothing
                    if precomputed_block.state_hash == dangling_branch.root.state_hash.0 {
                        return Ok(ExtensionType::BlockNotAdded);
                    }
                }

                // should be the only place we need this call
                self.best_chain = self.root_branch.longest_chain();
                if !branches_to_remove.is_empty() {
                    // if the root branch is newly connected to dangling branches
                    for (num_removed, index_to_remove) in branches_to_remove.iter().enumerate() {
                        self.dangling_branches.remove(index_to_remove - num_removed);
                    }
                    return Ok(ExtensionType::RootComplex);
                } else {
                    // if there aren't any branches that are connected
                    return Ok(ExtensionType::RootSimple);
                }
            }
            // }
        }

        let mut extension = None;
        for (index, dangling_branch) in self.dangling_branches.iter_mut().enumerate() {
            match (
                dangling_branch
                    .leaves
                    .iter()
                    .flat_map(|(_, x)| x.block.blockchain_length)
                    .fold(None, |acc, x| acc.max(Some(x))),
                dangling_branch.root.blockchain_length,
            ) {
                (Some(max_length), Some(min_length)) => {
                    // check incoming block is within the height bounds
                    if let Some(length) = precomputed_block.blockchain_length {
                        if max_length + 1 >= length && length + 1 >= min_length {
                            // simple reverse
                            if precomputed_block.state_hash == dangling_branch.root.parent_hash.0 {
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
                            if let Some(new_node_id) =
                                dangling_branch.simple_extension(precomputed_block)
                            {
                                extension = Some((index, new_node_id, ExtensionDirection::Forward));
                                break;
                            }

                            // if the block is already in a dangling branch, do nothing
                            if precomputed_block.state_hash == dangling_branch.root.state_hash.0 {
                                return Ok(ExtensionType::BlockNotAdded);
                            }
                        }
                    } else {
                        // we don't know the blockchain_length for the incoming block, so we can't discriminate

                        // simple reverse
                        if precomputed_block.state_hash == dangling_branch.root.parent_hash.0 {
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

                        // if the block is already in a dangling branch, do nothing
                        if precomputed_block.state_hash == dangling_branch.root.state_hash.0 {
                            return Ok(ExtensionType::BlockNotAdded);
                        }

                        // simple forward
                        if let Some(new_node_id) =
                            dangling_branch.simple_extension(precomputed_block)
                        {
                            extension = Some((index, new_node_id, ExtensionDirection::Forward));
                            break;
                        }
                    }
                }
                (None, None) => {
                    // simple reverse
                    if precomputed_block.state_hash == dangling_branch.root.parent_hash.0 {
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

                    // if the block is already in a dangling branch, do nothing
                    if precomputed_block.state_hash == dangling_branch.root.state_hash.0 {
                        return Ok(ExtensionType::BlockNotAdded);
                    }

                    // simple forward
                    if let Some(new_node_id) = dangling_branch.simple_extension(precomputed_block) {
                        extension = Some((index, new_node_id, ExtensionDirection::Forward));
                        break;
                    }
                }
                _ => unreachable!(),
            }
        }

        // if a dangling branch has been extended (forward or reverse) check for new connections to other dangling branches
        if let Some((extended_branch_index, new_node_id, direction)) = extension {
            let mut branches_to_update = Vec::new();
            for (index, dangling_branch) in self.dangling_branches.iter().enumerate() {
                if precomputed_block.state_hash == dangling_branch.root.parent_hash.0 {
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
                    // remove one for each index we see, i.e. n
                    self.dangling_branches.remove(index);
                }
                self.dangling_branches.push(extended_branch);
                return Ok(ExtensionType::DanglingComplex);
            } else {
                return match direction {
                    ExtensionDirection::Forward => Ok(ExtensionType::DanglingSimpleForward),
                    ExtensionDirection::Reverse => Ok(ExtensionType::DanglingSimpleReverse),
                };
            }
        }

        // block is added as a new dangling branch
        self.dangling_branches.push(
            Branch::new(
                precomputed_block,
                LedgerDiff::from_precomputed_block(precomputed_block),
            )
            .expect("cannot fail"),
        );
        Ok(ExtensionType::DanglingNew)
    }

    pub fn chain_commands(&self) -> Vec<Command> {
        self.best_chain
            .iter()
            .map(|leaf| leaf.block.state_hash.clone())
            .flat_map(|state_hash| self.block_store.as_ref().unwrap().get_block(&state_hash.0))
            .flatten()
            .flat_map(|precomputed_block| Command::from_precomputed_block(&precomputed_block))
            .collect()
    }

    pub fn best_ledger(&self) -> Option<&Ledger> {
        if let Some(head) = self.best_chain.last() {
            Some(head.get_ledger())
        } else {
            None
        }
    }
}

impl std::fmt::Debug for IndexerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== State ===")?;

        writeln!(f, "Root branch:")?;
        let mut tree = String::new();
        self.root_branch.branches.write_formatted(&mut tree)?;
        writeln!(f, "{tree}")?;

        writeln!(f, "Dangling branches:")?;
        for (n, branch) in self.dangling_branches.iter().enumerate() {
            writeln!(f, "Dangling branch {n}:")?;
            let mut tree = String::new();
            branch.branches.write_formatted(&mut tree)?;
            write!(f, "{tree}")?;
        }
        Ok(())
    }
}

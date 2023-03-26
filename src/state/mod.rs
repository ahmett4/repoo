use crate::block::{precomputed::PrecomputedBlock, store::BlockStore, Block, BlockHash};

use self::{
    branch::{Branch, Leaf},
    ledger::{command::Command, diff::LedgerDiff, Ledger},
};

pub mod branch;
pub mod ledger;

#[derive(Debug)]
pub struct State {
    pub best_chain: Vec<Leaf<Ledger>>,
    pub root_branch: Option<Branch<Ledger>>,
    pub dangling_branches: Vec<Branch<LedgerDiff>>,
    pub store: Option<BlockStore>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExtensionType {
    DanglingNew,
    DanglingSimpleForward,
    DanglingSimpleReverse,
    DanglingComplex,
    RootSimple,
    RootComplex,
}

pub enum ExtensionDirection {
    Forward,
    Reverse,
}

impl State {
    pub fn new(
        root: &PrecomputedBlock,
        blocks_path: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        let store = blocks_path.map(|path| BlockStore::new(path).unwrap());
        // genesis block => make new root_branch
        if root.state_hash == BlockHash::previous_state_hash(root).0 {
            // TODO get genesis ledger
            let genesis_ledger = Ledger::default();
            let block = Block::from_precomputed(root, 1);
            Ok(Self {
                best_chain: Vec::from([Leaf::new(block, genesis_ledger)]),
                root_branch: Some(Branch::<Ledger>::new_rooted(root)),
                dangling_branches: Vec::new(),
                store,
            })
        } else {
            Ok(Self {
                best_chain: Vec::new(),
                root_branch: None,
                dangling_branches: Vec::from([Branch::<LedgerDiff>::new_rooted(root)]),
                store,
            })
        }
    }

    pub fn add_block(&mut self, precomputed_block: &PrecomputedBlock) -> ExtensionType {
        // forward extension on root branch
        if let Some(mut root_branch) = self.root_branch.clone() {
            if let Some(max_length) = root_branch
                .leaves
                .iter()
                .flat_map(|(_, x)| x.block.blockchain_length)
                .fold(None, |acc, x| acc.max(Some(x)))
            {
                // check leaf heights first
                if max_length + 1 >= precomputed_block.blockchain_length.unwrap_or(0) {
                    if let Some(new_node_id) = root_branch.simple_extension(precomputed_block) {
                        let mut branches_to_remove = Vec::new();
                        // check if new block connects to a dangling branch
                        for (index, dangling_branch) in
                            self.dangling_branches.iter_mut().enumerate()
                        {
                            if precomputed_block.state_hash == dangling_branch.root.parent_hash.0 {
                                root_branch.merge_on(&new_node_id, dangling_branch);
                                branches_to_remove.push(index);
                            }
                        }

                        // should be the only place we need this call
                        self.best_chain = root_branch.longest_chain();
                        if !branches_to_remove.is_empty() {
                            // if the root branch is newly connected to dangling branches
                            for index_to_remove in branches_to_remove {
                                self.dangling_branches.remove(index_to_remove);
                            }
                            return ExtensionType::RootComplex;
                        } else {
                            // if there aren't any branches that are connected
                            return ExtensionType::RootSimple;
                        }
                    }
                }
            }
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
                let mut extended_branch = self.dangling_branches.remove(extended_branch_index);
                for dangling_branch_index in branches_to_update {
                    let index = if extended_branch_index < dangling_branch_index {
                        dangling_branch_index - 1
                    } else {
                        dangling_branch_index
                    };
                    let branch_to_update = self.dangling_branches.get_mut(index).unwrap();
                    extended_branch.merge_on(&new_node_id, branch_to_update);
                    self.dangling_branches.remove(index);
                }
                self.dangling_branches.push(extended_branch);
                return ExtensionType::DanglingComplex;
            } else {
                return match direction {
                    ExtensionDirection::Forward => ExtensionType::DanglingSimpleForward,
                    ExtensionDirection::Reverse => ExtensionType::DanglingSimpleReverse,
                };
            }
        }

        // block is added as a new dangling branch
        self.dangling_branches.push(
            Branch::new(
                precomputed_block,
                LedgerDiff::fom_precomputed_block(precomputed_block),
            )
            .expect("cannot fail"),
        );
        ExtensionType::DanglingNew
    }

    pub fn chain_commands(&self) -> Vec<Command> {
        self.best_chain
            .iter()
            .map(|leaf| leaf.block.state_hash.clone())
            .flat_map(|state_hash| self.store.as_ref().unwrap().get_block(&state_hash.0))
            .flatten()
            .flat_map(|precomputed_block| Command::from_precomputed_block(&precomputed_block))
            .collect()
    }
}

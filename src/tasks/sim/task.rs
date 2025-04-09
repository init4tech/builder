use crate::tasks::sim::SimFactory;
use trevm::revm::context::result::ResultAndState;

/// A task that simulates a transaction in the context of a block.
pub struct SimTask<Db, Insp> {
    factor: SimFactory<Db, Insp>,

    tx_eval: fn(&ResultAndState) -> u64,

    concurrency_limit: usize,
}

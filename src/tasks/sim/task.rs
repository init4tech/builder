use crate::tasks::{block::InProgressBlock, sim::SimFactory};
use alloy::consensus::TxEnvelope;
use tokio::{sync::mpsc::UnboundedReceiver, task::JoinSet};
use trevm::{
    db::cow::CacheOnWrite,
    helpers::Ctx,
    revm::{DatabaseRef, Inspector, context::result::ResultAndState},
};

/// A task that simulates a transaction in the context of a block.
pub struct SimTask<Db, Insp> {
    factor: SimFactory<Db, Insp>,

    tx_eval: fn(&ResultAndState) -> u64,

    concurrency_limit: usize,
}

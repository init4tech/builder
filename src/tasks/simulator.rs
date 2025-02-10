use super::{block::InProgressBlock, bundler::Bundle};
use crate::config::{BuilderConfig, WalletlessProvider};
use alloy::{
    consensus::TxEnvelope, eips::BlockId, network::Ethereum, providers::Provider,
    transports::BoxTransport,
};
use alloy_rlp::Encodable;
use eyre::Result;
use revm::{
    db::{AlloyDB, CacheDB},
    primitives::{ResultAndState, U256},
    DatabaseCommit,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{
    select,
    sync::mpsc::{self},
    task::JoinHandle,
};
use trevm::{
    revm::{primitives::EVMError, Database}, BlockDriver, NoopBlock, NoopCfg, Trevm, TrevmBuilder
};
use zenith_types::ZenithEthBundle;

/// Ethereum's slot time in seconds
pub const ETHEREUM_SLOT_TIME: u64 = 12;

/// Simulator wraps a trevm environment to a rollup provider to simulate transactions against that rollup state.
pub struct Simulator {
    pub ru_provider: WalletlessProvider,
    pub config: BuilderConfig,
}

/// Defines the SimulatorDatabase type for ease of use and clarityDefines the SimulatorDatabase type
pub type SimulatorDatabase = CacheDB<AlloyDB<BoxTransport, Ethereum, WalletlessProvider>>;

struct SimAndEvalResult {
    pub bundle: Bundle,
    pub score: U256,
    pub resultant_state: bool, // TODO
}

impl Simulator {
    /// Creates a new simulator at the latest block number.
    pub async fn new(ru_provider: WalletlessProvider, config: BuilderConfig) -> Result<Self> {
        Ok(Self { ru_provider, config })
    }

    /// Takes a cancellation channel `cancel`, a stream of bundles `inbound`, and an evaluator function `eval`
    /// and listens for incoming bundles to simulate and ingest them into an in progress block.
    /// - The evaluator function is applied to incoming bundles and they are scored accordingly.
    /// - The best scored bundles are assembled into a block candidate.
    /// - The best block candidate is returned when cancel is received.
    pub async fn run_simulation<F>(
        &self,
        mut deadline: Duration,
        inbound_bundles: &mut mpsc::UnboundedReceiver<Bundle>,
        inbound_txs: &mut mpsc::UnboundedReceiver<TxEnvelope>,
        eval: F,
    ) -> Result<InProgressBlock>
    where
        F: Fn(&ResultAndState, &ResultAndState) -> U256 + Send + 'static,
    {
        // Instantiate chain state at latest with trevm
        let db = self.get_latest_db().await?;
        let mut extractor = create_extractor::<SimulatorDatabase>();
        let current_state = extractor.trevm(db);

        let cancel = tokio::time::sleep(deadline);
        let mut included_bundles: Vec<Bundle> = Vec::new();
        let mut candidate_bundles: Vec<Bundle> = Vec::new();

        loop {
            select! {
                // Handle cancellation
                _ = cancel => {
                    let mut final_block: InProgressBlock = InProgressBlock::new();
                    // loop through bundles in block and ingest them
                    for bundle in included_bundles {
                        final_block.ingest_bundle(bundle);
                    }
                    return Ok(final_block)
                },
                // Handle bundle receive
                Some(bundle) = inbound_bundles.recv() => {
                    if included_bundles.contains(&bundle) {
                        // TODO: check if this is a replacement (same id, diff bundle contents)
                        // if it's a replacement, remove it from included_bundles, and allow it to be added back to candidate_bundles
                        // if it's NOT a replacement (same id, same bundle contents), leave it in included_bundles
                        todo!("handle replacement");
                    }
                    // remove any candidate bundles with the same uuid
                    candidate_bundles.retain(|b| b.id != bundle.id);
                    // push the new bundle
                    candidate_bundles.push(bundle);
                },
                Some(tx) = inbound_txs.recv() => {
                    // transform the tx into a bundle
                    // TODO: do we really want to do this? 
                    // should we actually write simulator logic that handles txs differently?
                    let bundle = Bundle::from(tx);
                    // ensure the bundle is not already included or in the candidate bundles
                    if included_bundles.contains(&bundle) {
                        // TODO: check if this is a replacement (same id, diff bundle contents)
                        // if it's a replacement, remove it from included_bundles, and allow it to be added back to candidate_bundles
                        // if it's NOT a replacement (same id, same bundle contents), leave it in included_bundles
                        todo!("handle replacement");
                    }
                    // remove any candidate bundles with the same uuid
                    candidate_bundles.retain(|b| b.id != bundle.id);
                    // push the new bundle
                    candidate_bundles.push(bundle);
                },
                Some(best_bundle) = self.sim_and_eval_in_parallel(&current_state, &candidate_bundles) => {
                    // add the best bundle into the block
                    included_bundles.push(best_bundle.bundle);
                    // remove it from candidate bundles
                    candidate_bundles.retain(|b| b.id != best_bundle.bundle.id);
                    // update the resulting state, so next evaluation runs against new state
                    // TODO
                    // current_state = best_bundle.resultant_state;
                }
            }
        }
    }

    async fn sim_and_eval_in_parallel(current_state, bundles) => SimAndEvalResult {
    //     // for each bundle, spawn a simulation of that bundle against current state
    //    let futs = bundles.foreach(bundle => spawn(simulate_and_evaluate(current_state, bundle)));
    
    //    // await the results in parallel
    //    let results = futs.await_all;
    
    //    // sort the best score to pick the best result
    //    sort_by_score(results);
    //    best = results[0];
    //    return best;
    }
    
    // async fn simulate_and_evaluate(current_state, bundle) => SimAndEvalResult {
    //     // apply bundle to state
    //     let resultant_state = current_state.run(bundle);
    
    //     // run evaluator function
    //     let score = evaluator(current_state, resultant_state);
    
    //     // return results
    //     return {bundle, score, resultant_state}
    // }

    /// Returns a prepared Simulator database out of the ru_provider at the latest block number
    pub async fn get_latest_db(&self) -> eyre::Result<SimulatorDatabase> {
        let latest = self.ru_provider.clone().get_block_number().await?;
        if let Some(db) = AlloyDB::new(self.ru_provider.clone(), BlockId::from(latest)) {
            Ok(CacheDB::new(db))
        } else {
            Err(eyre::eyre!("failed to create alloyDB from ru_provider"))
        }
    }

    /// Simulates a bundle against latest tip state
    pub async fn simulate_bundle(&self, bundle: ZenithEthBundle) -> eyre::Result<()> {
        let db = self.get_latest_db().await?;
        let mut extractor = create_extractor::<SimulatorDatabase>();
        let trevm_env = extractor.trevm(db);
        let mut driver = extractor.extract(todo!());
        Ok(())
    }

    /// Spawn a new Simulator that receives bundles and transactions and simulates them into finalized
    /// blocks for later submission to the network.
    pub async fn spawn(
        self,
        mut inbound_bundles: mpsc::UnboundedReceiver<Bundle>,
        mut inbound_txs: mpsc::UnboundedReceiver<TxEnvelope>,
        submit_channel: mpsc::UnboundedSender<InProgressBlock>,
    ) -> JoinHandle<()> {
        let jh = tokio::spawn(async move {
            let timer = Timer::new(self.config.clone());

            loop {
                // Block building loop
                // TODO: Trevm DB instantiation must respect block timing
                let next_target_slot = timer.clone().secs_to_next_target();
                let deadline = Duration::from_secs(next_target_slot);

                // Kick off simulation with given deadline
                tracing::info!(deadline = ?deadline, "starting simulation");
                // TODO: Handles last known best block in simulation loop
                // let mut last_known_good: Option<InProgressBlock> = None;
                // let mut candidate_block = InProgressBlock::default();
                todo!()
            }
        });
        jh
    }

    /// Builds a block by receiving and simulating bundles and transactions within the given deadline
    /// and then returning the latest candidate block that has been successfully simulated
    pub async fn build_block(
        self,
        deadline: Duration,
        inbound_bundles: &mut mpsc::UnboundedReceiver<Bundle>,
        inbound_txs: &mut mpsc::UnboundedReceiver<TxEnvelope>,
    ) -> InProgressBlock {
        let result = self.run_simulation(deadline, inbound_bundles, inbound_txs, evaluator).await;
        match result {
            Ok(finalized) => finalized,
            Err(_) => InProgressBlock::default(),
        }
    }
}

/// Evaluates the value of a bundle and returns its score for sorting purposes
fn evaluator(prev: &ResultAndState, proposed: &ResultAndState) -> U256 {
    todo!()
}

/// Timer implements the logic for predicting time to next slot
#[derive(Clone)]
pub struct Timer {
    pub config: BuilderConfig,
}

impl Timer {
    // Create a new block builder with the given config.
    pub fn new(config: BuilderConfig) -> Self {
        Self { config }
    }

    // Calculate the duration in seconds until the beginning of the next block slot.
    fn secs_to_next_slot(&self) -> u64 {
        let curr_timestamp: u64 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let current_slot_time = (curr_timestamp - self.config.chain_offset) % ETHEREUM_SLOT_TIME;
        (ETHEREUM_SLOT_TIME - current_slot_time) % ETHEREUM_SLOT_TIME
    }

    // Add a buffer to the beginning of the block slot.
    pub fn secs_to_next_target(&self) -> u64 {
        self.secs_to_next_slot() + self.config.target_slot_time
    }
}

/// Creates an extractor from a generic Db that gives you access to a trevm environment.
pub fn create_extractor<Db>() -> impl BlockExtractor<(), Db>
where
    Db: Database + DatabaseCommit + Send + 'static,
{
    BuilderBlockExtractor {}
}

pub trait BlockExtractor<Ext, Db: Database + DatabaseCommit>: Send + Sync + 'static {
    type Driver: BlockDriver<Ext, Error<Db>: core::error::Error>;

    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, Ext, Db>;

    fn extract(&mut self, bytes: &[u8]) -> Self::Driver;
}

impl<Db> BlockExtractor<(), Db> for BuilderBlockExtractor
where
    Db: Database + DatabaseCommit + Send + 'static,
{
    type Driver = Block;

    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, (), Db> {
        trevm::revm::EvmBuilder::default().with_db(db).build_trevm().fill_cfg(&NoopCfg)
    }

    fn extract(&mut self, bytes: &[u8]) -> Self::Driver {
        let txs: Vec<TxEnvelope> =
            alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap_or_default();
        Block(txs, NoopBlock)
    }
}

/// Block extractor for the Builder
pub struct BuilderBlockExtractor {}

pub struct Block(Vec<TxEnvelope>, NoopBlock);

impl<Ext> BlockDriver<Ext> for Block {
    type Block = NoopBlock;
    type Error<Db: Database> = Error<Db>;

    fn block(&self) -> &Self::Block {
        &NoopBlock
    }

    fn run_txns<'a, Db: Database + DatabaseCommit>(
        &mut self,
        mut trevm: trevm::EvmNeedsTx<'a, Ext, Db>,
    ) -> trevm::RunTxResult<'a, Ext, Db, Self> {
        for tx in self.0.iter() {
            if tx.recover_signer().is_ok() {
                todo!()
            }
        }
        Ok(trevm)
    }

    fn post_block<Db: Database + DatabaseCommit>(
        &mut self,
        _trevm: &trevm::EvmNeedsBlock<'_, Ext, Db>,
    ) -> Result<(), Self::Error<Db>> {
        Ok(())
    }
}

/// Error implementation
pub struct Error<Db: Database>(EVMError<Db::Error>);

impl<Db> From<EVMError<Db::Error>> for Error<Db>
where
    Db: Database,
{
    fn from(e: EVMError<Db::Error>) -> Self {
        Self(e)
    }
}

impl<Db: Database> core::error::Error for Error<Db> {}

impl<Db: Database> core::fmt::Debug for Error<Db> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Error")
    }
}

impl<Db: Database> core::fmt::Display for Error<Db> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Error")
    }
}

use alloy::consensus::TxEnvelope;
use alloy::primitives::U256;
use revm::{db::CacheDB, DatabaseRef};
use std::{convert::Infallible, sync::Arc};
use tokio::{select, sync::mpsc::UnboundedReceiver};

use trevm::{
    self,
    db::{ConcurrentState, ConcurrentStateInfo},
    revm::{
        primitives::{EVMError, ResultAndState},
        Database, DatabaseCommit,
    },
    BlockDriver, DbConnect, EvmFactory, NoopBlock, NoopCfg, TrevmBuilder, Tx,
};

pub struct Best<T, Score: PartialOrd + Ord = U256> {
    pub tx: Arc<T>,
    pub result: ResultAndState,
    pub score: Score,
}

/// SimBlock wraps an array of SimBundles
pub struct SimBlock(pub Vec<SimBundle>);

/// Binds a database and a simulation extension together
#[derive(Clone)]
pub struct SimulatorFactory<Db, Ext> {
    pub db: Db,
    pub ext: Ext,
}

impl<'a, Db, Ext> SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Send + Sync + 'static,
{
    pub fn new(db: Db, ext: Ext) -> Self {
        Self { db, ext }
    }

    /// Spawns a trevm simulator
    pub fn spawn<T, F>(
        self,
        mut inbound_tx: UnboundedReceiver<Arc<SimTxEnvelope>>,
        mut inbound_bundle: UnboundedReceiver<Arc<SimBundle>>,
        evaluator: Arc<F>,
        deadline: tokio::time::Instant,
    ) -> tokio::task::JoinHandle<Option<Best<SimBlock>>>
    where
        T: Tx + Send + Sync + 'static,
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
        Ext: Send + Sync + Clone + 'static,
        <Db as DatabaseRef>::Error: Send,
    {
        let jh = tokio::spawn(async move {
            let mut best: Option<Best<SimBlock>> = None;

            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            loop {
                select! {
                    _ = &mut sleep => {
                        break;
                    },
                    tx = inbound_tx.recv() => {
                        tracing::debug!("received tx");

                        if let Some(inbound_tx) = tx {
                            tracing::debug!("handling inbound tx");

                            let trevm_instance = match self.create() {
                                Ok(instance) => instance,
                                Err(e) => {
                                    tracing::error!(e = ?e, "Failed to create trevm instance");
                                    continue
                                }
                            };

                            self.handle_inbound_tx(inbound_tx, evaluator.clone(), trevm_instance);
                        }
                    }
                    bundle = inbound_bundle.recv() => {
                        if let Some(_bundle) = bundle {
                            println!("handling inbound bundle");

                            let _trevm_instance = match self.create() {
                                Ok(instance) => instance,
                                Err(e) => {
                                    tracing::error!(e = ?e, "Failed to create trevm instance");
                                    continue
                                }
                            };

                            todo!()
                        }
                    }
                }
            }

            best
        });

        jh
    }

    /// simulates an inbound tx and applies its state if it's successfully simualted
    pub fn handle_inbound_tx<T, F>(
        &self,
        tx: Arc<T>,
        evaluator: Arc<F>,
        trevm_instance: trevm::EvmNeedsCfg<'_, (), ConcurrentState<CacheDB<Db>>>,
    ) -> Option<Best<SimBlock>>
    where
        T: Tx,
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
    {
        let mut block_driver = SimBundle(vec![todo!()], NoopBlock);

        // Configure and run the transaction
        let result = trevm_instance.fill_cfg(&NoopCfg).drive_block(&mut block_driver);

        match result {
            Ok(result) => {
                // TODO: Run the evaluator on the completed state, returning the Best block
                // let score = evaluator(result);
                // Some(Best {
                //     tx,
                //     result: result_and_state,
                //     score,
                // })
                todo!()
            }
            Err(e) => {
                tracing::error!("Failed to drive block: {:?}", e);
                None
            }
        }
    }

    /// Simulates an inbound bundle and applies its state if it's successfully simulated
    pub fn handle_inbound_bundle<T: Tx, F>(&self, bundle: Arc<Vec<T>>, evaluator: Arc<F>) {
        println!("received tx");
        todo!()
    }
}

// Wraps a Db into an EvmFactory compatible [`Database`]
impl<'a, Db, Ext> DbConnect<'a> for SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Sync + Send + 'static,
    Ext: Sync + Clone,
{
    type Database = ConcurrentState<CacheDB<Db>>;
    type Error = Infallible;

    fn connect(&'a self) -> Result<Self::Database, Self::Error> {
        let cache: CacheDB<Db> = CacheDB::new(self.db.clone());
        let concurrent_db = ConcurrentState::new(cache, ConcurrentStateInfo::default());
        Ok(concurrent_db)
    }
}

impl<'a, Db, Ext> EvmFactory<'a> for SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Sync + Send + 'static,
    Ext: Sync + Clone,
{
    type Ext = ();

    /// Create makes a [`ConcurrentState`] database by calling connect
    fn create(&'a self) -> Result<trevm::EvmNeedsCfg<'a, Self::Ext, Self::Database>, Self::Error> {
        let concurrent_db = self.connect()?;
        let t = trevm::revm::EvmBuilder::default().with_db(concurrent_db).build_trevm();
        Ok(t)
    }
}

///
/// Extractor
///

/// A trait for extracting transactions from a block.
pub trait BlockExtractor<Ext, Db: Database + DatabaseCommit>: Send + Sync + 'static {
    type Driver: BlockDriver<Ext, Error<Db>: core::error::Error>;

    /// Instantiate an configure a new [`trevm`] instance.
    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, Ext, Db>;

    /// Extracts transactions from the source.
    ///
    /// Extraction is infallible. Worst case it should return a no-op driver.
    fn extract(&mut self, bytes: &[u8]) -> Self::Driver;
}

/// An implementation of BlockExtractor for Simulation purposes
#[derive(Clone)]
pub struct SimulatorExtractor {}

/// SimulatorExtractor implements a block extractor and trevm block driver
/// for simulating and successively applying state updates from transactions.
impl<Db> BlockExtractor<(), Db> for SimulatorExtractor
where
    Db: Database + DatabaseCommit + Send + Sync + 'static,
{
    type Driver = SimBundle;

    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, (), Db> {
        trevm::revm::EvmBuilder::default().with_db(db).build_trevm().fill_cfg(&NoopCfg)
    }

    fn extract(&mut self, bytes: &[u8]) -> Self::Driver {
        // TODO: Should this use SimBundle instead of Vec<TxEnvelope>?
        #[allow(clippy::useless_asref)]
        let txs: Vec<TxEnvelope> =
            alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap_or_default();
        let sim_txs = txs.iter().map(|f| SimTxEnvelope(f.clone())).collect();
        SimBundle(sim_txs, NoopBlock)
    }
}

pub struct SimBundle(Vec<SimTxEnvelope>, NoopBlock);

pub struct SimTxEnvelope(pub TxEnvelope);

impl From<&[u8]> for SimTxEnvelope {
    fn from(bytes: &[u8]) -> Self {
        let tx: TxEnvelope = alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap();
        SimTxEnvelope(tx)
    }
}

impl Tx for SimTxEnvelope {
    fn fill_tx_env(&self, tx_env: &mut revm::primitives::TxEnv) {
        tracing::info!("fillng tx env {:?}", tx_env); // Possible cause
        let revm::primitives::TxEnv { .. } = tx_env;
    }
}

impl<Ext> BlockDriver<Ext> for SimBundle {
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
            if tx.0.recover_signer().is_ok() {
                let sim_tx = SimTxEnvelope(tx.0.clone());
                let t = match trevm.run_tx(&sim_tx) {
                    Ok(t) => {
                        print!(
                            "successfully ran transaction - gas used {}",
                            t.result_and_state().result.gas_used()
                        );

                        t
                    }
                    Err(e) => {
                        if e.is_transaction_error() {
                            return Ok(e.discard_error());
                        } else {
                            return Err(e.err_into());
                        }
                    }
                };
                (_, trevm) = t.accept();
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

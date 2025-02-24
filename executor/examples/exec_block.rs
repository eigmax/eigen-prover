#![allow(dead_code)]
use ethers_core::types::BlockId;
use ethers_providers::Middleware;
use ethers_providers::{Http, Provider};
//use revm::inspectors::TracerEip3155;
//use ruint::{aliases::*, uint, Uint};

use revm::db::{CacheDB, EmptyDB, EthersDB};
use revm::primitives::{Address, Env, ResultAndState, SpecId, TransactTo, U256};
use revm::Database;
use revm::DatabaseCommit;
use revm::{handler::Handler, Context};

use std::env as stdenv;
use std::io::BufWriter;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;

macro_rules! local_fill {
    ($left:expr, $right:expr, $fun:expr) => {
        if let Some(right) = $right {
            $left = $fun(right.0)
        }
    };
    ($left:expr, $right:expr) => {
        if let Some(right) = $right {
            $left = Address::from(right.as_fixed_bytes())
        }
    };
}

struct FlushWriter {
    writer: Arc<Mutex<BufWriter<std::fs::File>>>,
}

impl FlushWriter {
    fn new(writer: Arc<Mutex<BufWriter<std::fs::File>>>) -> Self {
        Self { writer }
    }
}

impl Write for FlushWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.lock().unwrap().flush()
    }
}

/// Usage: NO=457 cargo run --release --example prove_block
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env_block_number = stdenv::var("NO").unwrap_or(String::from("0"));
    let block_number: u64 = env_block_number.parse().unwrap();

    // Create ethers client and wrap it in Arc<M>
    // can run a hardhat node
    let client = Provider::<Http>::try_from("http://localhost:8545").unwrap();
    //let client = Provider::<Http>::try_from(
    //    "https://mainnet.infura.io/v3/c60b0bb42f8a4c6481ecd229eddaca27",
    //)?;

    let client = Arc::new(client);

    // Params
    let chain_id: u64 = 1;
    //let block_number = 10889447;

    // Fetch the transaction-rich block
    let block = match client.get_block_with_txs(block_number).await {
        Ok(Some(block)) => block,
        Ok(None) => panic!("Block not found"),
        Err(error) => panic!("Error: {:?}", error),
    };
    println!("Fetched block number: {:?}", block.number.unwrap());
    let previous_block_number = block_number - 1;

    // Use the previous block state as the db with caching
    let prev_id: BlockId = previous_block_number.into();
    // SAFETY: This cannot fail since this is in the top-level tokio runtime
    let mut ethersdb = EthersDB::new(Arc::clone(&client), Some(prev_id)).unwrap();

    let mut cache_db = CacheDB::new(EmptyDB::default());
    // get pre
    for tx in &block.transactions {
        let from_acc = Address::from(tx.from.as_fixed_bytes());
        // query basic properties of an account incl bytecode
        let acc_info = ethersdb.basic(from_acc).unwrap().unwrap();
        println!("acc_info: {} => {:?}", from_acc, acc_info);
        cache_db.insert_account_info(from_acc, acc_info);

        if tx.to.is_some() {
            let to_acc = Address::from(tx.to.unwrap().as_fixed_bytes());
            let acc_info = ethersdb.basic(to_acc).unwrap().unwrap();
            println!("to_info: {} => {:?}", to_acc, acc_info);
            // setup storage
            /*
            uint!{
                for slot in [0x2_U256, 0x82440beeb8ea7bdc8fb6c47af3bdfbce49c97853988e80d3269a2ccae791587a_U256] {
                    let slot = U256::from(slot);
                    if acc_info.code.as_ref().unwrap().len() > 0 {
                        // query value of storage slot at account address
                        let value = ethersdb.storage(to_acc, slot).unwrap();
                        println!("slot:{}, value: {:?}", slot, value);

                        cache_db
                            .insert_account_storage(to_acc, slot, value)
                            .unwrap();
                    }
                }
            }
            */
            cache_db.insert_account_info(to_acc, acc_info);
        }
    }

    let ctx = Context::new_with_db(cache_db);
    //ctx.evm.env = Box::new(env.clone());
    let handler = Handler::mainnet_with_spec(SpecId::FRONTIER);
    let mut evm = revm::Evm::new(ctx, handler);

    let mut env = Env::default();
    if let Some(number) = block.number {
        let nn = number.0[0];
        env.block.number = U256::from(nn);
    }
    local_fill!(env.block.coinbase, block.author);
    local_fill!(env.block.timestamp, Some(block.timestamp), U256::from_limbs);
    local_fill!(
        env.block.difficulty,
        Some(block.difficulty),
        U256::from_limbs
    );
    local_fill!(env.block.gas_limit, Some(block.gas_limit), U256::from_limbs);
    if let Some(base_fee) = block.base_fee_per_gas {
        local_fill!(env.block.basefee, Some(base_fee), U256::from_limbs);
    }

    let txs = block.transactions.len();
    println!("Found {txs} transactions.");

    let elapsed = std::time::Duration::ZERO;

    // Create the traces directory if it doesn't exist
    std::fs::create_dir_all("traces").expect("Failed to create traces directory");

    // Fill in CfgEnv
    env.cfg.chain_id = chain_id;
    let mut all_result = vec![];
    for tx in block.transactions {
        env.tx.caller = Address::from(tx.from.as_fixed_bytes());
        env.tx.gas_limit = tx.gas.as_u64();
        local_fill!(env.tx.gas_price, tx.gas_price, U256::from_limbs);
        local_fill!(env.tx.value, Some(tx.value), U256::from_limbs);
        env.tx.data = tx.input.0.into();
        let mut gas_priority_fee = U256::ZERO;
        local_fill!(
            gas_priority_fee,
            tx.max_priority_fee_per_gas,
            U256::from_limbs
        );
        env.tx.gas_priority_fee = Some(gas_priority_fee);
        env.tx.chain_id = Some(chain_id);
        env.tx.nonce = Some(tx.nonce.as_u64());
        if let Some(access_list) = tx.access_list {
            env.tx.access_list = access_list
                .0
                .into_iter()
                .map(|item| {
                    let new_keys: Vec<U256> = item
                        .storage_keys
                        .into_iter()
                        .map(|h256| U256::from_le_bytes(h256.0))
                        .collect();
                    (Address::from(item.address.as_fixed_bytes()), new_keys)
                })
                .collect();
        } else {
            env.tx.access_list = Default::default();
        }

        env.tx.transact_to = match tx.to {
            Some(to_address) => TransactTo::Call(Address::from(to_address.as_fixed_bytes())),
            None => TransactTo::create(),
        };

        evm.context.evm.env = Box::new(env.clone());

        /*
        // Construct the file writer to write the trace to
        let tx_number = tx.transaction_index.unwrap().0[0];
        let file_name = format!("traces/{}.json", tx_number);
        let write = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(file_name);
        let inner = Arc::new(Mutex::new(BufWriter::new(
            write.expect("Failed to open file"),
        )));
        let writer = FlushWriter::new(Arc::clone(&inner));

        // Inspect and commit the transaction to the EVM
        let inspector = TracerEip3155::new(Box::new(writer), true, true);
        if let Err(error) = evm.inspect_commit(inspector) {
            println!("Got error: {:?}", error);
        }

        // Flush the file writer
        inner.lock().unwrap().flush().expect("Failed to flush file");
        */
        //evm.transact_commit().unwrap();
        let result = evm.transact().unwrap();
        evm.context.evm.db.commit(result.state.clone());
        let txbytes = serde_json::to_vec(&env.tx).unwrap();
        all_result.push((txbytes, env.tx.data, env.tx.value, result));
    }
    for (k, v) in &evm.context.evm.db.accounts {
        println!("state: {}=>{:?}", k, v);
        if !v.storage.is_empty() {
            for (k, v) in v.storage.iter() {
                println!("slot => storage: {}=>{}", k, v);
            }
        }
    }
    // get `post: BTreeMap<SpecName, Vec<Test>>`
    for res in &all_result {
        let (txbytes, data, value, ResultAndState { result, state }) = res;
        {
            // 1. expect_exception: Option<String>,
            println!("expect_exception: {:?}", result.is_success());
            // indexes: TxPartIndices,
            println!(
                "indexes: data{:?}, value: {}, gas: {}",
                data,
                value,
                result.gas_used()
            );
            println!("output: {:?}", result.output());

            // TODO: hash: B256, // post state root
            //let hash = serde_json::to_vec(&state).unwrap();
            //println!("hash: {:?}", state);
            // post_state: HashMap<Address, AccountInfo>,
            println!("post_state: {:?}", state);
            // logs: B256,
            println!("logs: {:?}", result.logs());
            // txbytes: Option<Bytes>,
            println!("txbytes: {:?}", txbytes);
        }
    }

    println!(
        "Finished execution. Total CPU time: {:.6}s",
        elapsed.as_secs_f64()
    );
    Ok(())
}

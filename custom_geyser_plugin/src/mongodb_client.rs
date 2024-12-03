/// A concurrent implementation for writing accounts into the MongoDB in parallel.
use {
    crate::geyser_plugin_mongodb::{GeyserPluginMongoDBConfig, GeyserPluginMongoDbError}, chrono::Utc, crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender}, log::*, mongodb::{bson::doc, options::{ClientOptions, Tls, TlsOptions}, Client }, openssl::ssl::{SslConnector, SslFiletype, SslMethod}, serde::{Deserialize, Serialize}, solana_geyser_plugin_interface::geyser_plugin_interface::{
        GeyserPluginError, ReplicaAccountInfoV3, ReplicaBlockInfoV3, SlotStatus,
    }, solana_measure::measure::Measure, solana_metrics::*, solana_sdk::{instruction::Instruction, timing::AtomicInterval}, solana_transaction_status::TransactionStatus, std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        thread::{self, sleep, Builder, JoinHandle},
        time::Duration,
    }
};

/// The maximum asynchronous requests allowed in the channel to avoid excessive
/// memory usage. The downside -- calls after this threshold is reached can get blocked.
const MAX_ASYNC_REQUESTS: usize = 40960;
const SAFE_BATCH_STARTING_SLOT_CUSHION: u64 = 2 * 40960;
const DEFAULT_MONGO_DB_PORT: u16 = 27017;
const DEFAULT_THREADS_COUNT: usize = 100;
const DEFAULT_ACCOUNTS_INSERT_BATCH_SIZE: usize = 10;
const ACCOUNT_COLUMN_COUNT: usize = 10;
const DEFAULT_PANIC_ON_DB_ERROR: bool = false;
const DEFAULT_STORE_ACCOUNT_HISTORICAL_DATA: bool = false;

//MONGODB_CLIENT_ACCOUNT_INDEX
const TOKEN_INDEX_COLUMN_COUNT: usize = 3;
/// Struct for the secondary index for both token account's owner and mint index,
pub struct TokenSecondaryIndexEntry {
    /// In case of token owner, the secondary key is the Pubkey of the owner and in case of
    /// token index the secondary_key is the Pubkey of mint.
    secondary_key: Vec<u8>,

    /// The Pubkey of the account
    account_key: Vec<u8>,

    /// Record the slot at which the index entry is created.
    slot: i64,
}


//MONGODB_CLIENT_TRANSACTION
const MAX_TRANSACTION_STATUS_LEN: usize = 256;

#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbCompiledInstruction {
    pub program_id_index: i16,
    pub accounts: Vec<i16>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize,Deserialize)]

pub struct DbInnerInstructions {
    pub index: i16,
    pub instructions: Vec<DbCompiledInstruction>,
}

#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbTransactionTokenBalance {
    pub account_index: i16,
    pub mint: String,
    pub ui_token_amount: Option<f64>,
    pub owner: String,
}

#[derive(Clone, Debug, Eq, Serialize,Deserialize, PartialEq)]
pub enum DbRewardType {
    Fee,
    Rent,
    Staking,
    Voting,
}


#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbReward {
    pub pubkey: String,
    pub lamports: i64,
    pub post_balance: i64,
    pub reward_type: Option<DbRewardType>,
    pub commission: Option<i16>,
}


#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbTransactionStatusMeta {
    pub error: Option<DbTransactionError>,
    pub fee: i64,
    pub pre_balances: Vec<i64>,
    pub post_balances: Vec<i64>,
    pub inner_instructions: Option<Vec<DbInnerInstructions>>,
    pub log_messages: Option<Vec<String>>,
    pub pre_token_balances: Option<Vec<DbTransactionTokenBalance>>,
    pub post_token_balances: Option<Vec<DbTransactionTokenBalance>>,
    pub rewards: Option<Vec<DbReward>>,
}


#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbTransactionMessageHeader {
    pub num_required_signatures: i16,
    pub num_readonly_signed_accounts: i16,
    pub num_readonly_unsigned_accounts: i16,
}

#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbTransactionMessage {
    pub header: DbTransactionMessageHeader,
    pub account_keys: Vec<Vec<u8>>,
    pub recent_blockhash: Vec<u8>,
    pub instructions: Vec<DbCompiledInstruction>,
}

#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbTransactionMessageAddressTableLookup {
    pub account_key: Vec<u8>,
    pub writable_indexes: Vec<i16>,
    pub readonly_indexes: Vec<i16>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DbTransactionMessageV0 {
    pub header: DbTransactionMessageHeader,
    pub account_keys: Vec<Vec<u8>>,
    pub recent_blockhash: Vec<u8>,
    pub instructions: Vec<DbCompiledInstruction>,
    pub address_table_lookups: Vec<DbTransactionMessageAddressTableLookup>,
}

#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbLoadedAddresses {
    pub writable: Vec<Vec<u8>>,
    pub readonly: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize,Deserialize)]
pub struct DbLoadedMessageV0 {
    pub message: DbTransactionMessageV0,
    pub loaded_addresses: DbLoadedAddresses,
}

pub struct DbTransaction {
    pub signature: Vec<u8>,
    pub is_vote: bool,
    pub slot: i64,
    pub message_type: i16,
    pub legacy_message: Option<DbTransactionMessage>,
    pub v0_loaded_message: Option<DbLoadedMessageV0>,
    pub message_hash: Vec<u8>,
    pub meta: DbTransactionStatusMeta,
    pub signatures: Vec<Vec<u8>>,
    /// Useful for deciphering the order of transaction within a block
    /// Given a slot, the transaction with a smaller write_version appears
    /// before transactions with higher write_versions in a shred.
    pub write_version: i64,
    pub index: i64,
}


struct MongodbClientWrapper {
    client: mongodb::Client,
    accounts_collection:mongodb::Collection<DbAccountInfo>,
    slots_collection:mongodb::Collection<SlotMetadata>,
    transactions_colection:mongodb::Collection<TransactionLog>,
    token_owner_index_collection: Option<mongodb::Collection<TokenSecondaryIndexEntry>>,
    token_mint_index_collection: Option<mongodb::Collection<TokenSecondaryIndexEntry>>,
}

pub struct SimpleMongoDbClient {
    batch_size: usize,
    slots_at_startup: HashSet<u64>, //Hashset may consume sig memory if many slots are processed at startup. consider using a bitmap here?
    pending_account_updates: Vec<DbAccountInfo>,
    index_token_owner: bool,
    index_token_mint: bool,
    pending_token_owner_index: Vec<TokenSecondaryIndexEntry>,
    pending_token_mint_index: Vec<TokenSecondaryIndexEntry>,
    client: tokio::sync::Mutex<MongodbClientWrapper>, //allow thread-safe access to client wrapper
}

struct MongodbClientWorker {
    client: SimpleMongoDbClient,
    /// Indicating if accounts notification during startup is done.
    is_startup_done: bool,
}

#[derive(Clone,Debug)]
pub struct SlotMetadata{
    pub slot: u64,
    pub parent:Option<String>, //enables reconstruction of slot tree
    pub status: SlotStatus,
    pub blockhash:Option<String>,
    pub leader:Option<u8>, //None for orphaned slots
    pub timestamp:Option<u8>
}

#[derive(Clone,Debug, Serialize, Deserialize)]
pub struct TransactionLog{
    pub signature:String,
    pub slot: u64,
    pub status:TransactionStatus,
    pub instructions: Vec<Instruction>,
    pub logs: Vec<String>,
    pub fee: u64,
    pub pre_balances:Vec<u64>,
    pub post_balances:Vec<u64>,
    pub account:Vec<String>
}



#[derive(Clone, PartialEq, Debug)]
pub struct DbAccountInfo {
    pub pubkey: Vec<u8>, //using fixed-sized array, [u8; 32] for pubkeys may improve cache locality?
    pub lamports: i64,
    pub owner: Vec<u8>,
    pub executable: bool,
    pub rent_epoch: i64,
    pub data: Vec<u8>,
    pub slot: i64,
    pub write_version: i64,
    pub txn_signature: Option<Vec<u8>>,
}


impl DbAccountInfo {
    fn new<T: ReadableAccountInfo>(account: &T, slot: u64) -> DbAccountInfo {
        let data = account.data().to_vec();
        Self {
            pubkey: account.pubkey().to_vec(),
            lamports: account.lamports(),
            owner: account.owner().to_vec(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
            data,
            slot: slot as i64,
            write_version: account.write_version(),
            txn_signature: account.txn_signature().map(|v| v.to_vec()),
        }
    }
}
pub trait ReadableAccountInfo: Sized {
    fn pubkey(&self) -> &[u8];
    fn owner(&self) -> &[u8];
    fn lamports(&self) -> i64;
    fn executable(&self) -> bool;
    fn rent_epoch(&self) -> i64;
    fn data(&self) -> &[u8];
    fn write_version(&self) -> i64;
    fn txn_signature(&self) -> Option<&[u8]>;
}

impl ReadableAccountInfo for DbAccountInfo {
    fn pubkey(&self) -> &[u8] {
        &self.pubkey
    }

    fn owner(&self) -> &[u8] {
        &self.owner
    }

    fn lamports(&self) -> i64 {
        self.lamports
    }

    fn executable(&self) -> bool {
        self.executable
    }

    fn rent_epoch(&self) -> i64 {
        self.rent_epoch
    }

    fn data(&self) -> &[u8] {
        &self.data
    }

    fn write_version(&self) -> i64 {
        self.write_version
    }

    fn txn_signature(&self) -> Option<&[u8]> {
        self.txn_signature.as_deref()
    }
}

impl<'a> ReadableAccountInfo for ReplicaAccountInfoV3<'a> {
    fn pubkey(&self) -> &[u8] {
        self.pubkey
    }

    fn owner(&self) -> &[u8] {
        self.owner
    }

    fn lamports(&self) -> i64 {
        self.lamports as i64
    }

    fn executable(&self) -> bool {
        self.executable
    }

    fn rent_epoch(&self) -> i64 {
        self.rent_epoch as i64
    }

    fn data(&self) -> &[u8] {
        self.data
    }

    fn write_version(&self) -> i64 {
        self.write_version as i64
    }

    fn txn_signature(&self) -> Option<&[u8]> {
        self.txn.map(|v| v.signature().as_ref())
    }
}

pub trait MongoDBClient {
    fn join(&mut self) -> thread::Result<()> {
        Ok(())
    }
    fn update_account(
        &mut self,
        account: DbAccountInfo,
        is_startup: bool,
    ) -> Result<(), GeyserPluginError>;

    fn update_slot_status(
        &mut self,
        slot: u64,
        parent: Option<u64>,
        status: SlotStatus,
    ) -> Result<(), GeyserPluginError>;

    fn notify_end_of_startup(&mut self) -> Result<(), GeyserPluginError>;

    fn log_transaction(
        &mut self,
        transaction_log_info: LogTransactionRequest,
    ) -> Result<(), GeyserPluginError>;

    fn update_block_metadata(
        &mut self,
        block_info: UpdateBlockMetadataRequest,
    ) -> Result<(), GeyserPluginError>;
}

impl SimpleMongoDbClient {
    
}
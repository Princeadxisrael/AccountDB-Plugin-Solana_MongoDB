/// A concurrent implementation for writing accounts into the MongoDB in parallel.
use {
    crate::geyser_plugin_mongodb::{GeyserPluginMongoDBConfig, GeyserPluginMongoDbError}, 
    chrono::Utc, 
    crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender}, 
    log::*, 
    mongodb::{bson::doc, options::{ClientOptions, Tls, TlsOptions}, Client}, 
    openssl::ssl::{SslConnector, SslFiletype, SslMethod}, 
    serde::{Deserialize, Serialize}, 
    solana_geyser_plugin_interface::geyser_plugin_interface::{
        GeyserPluginError, ReplicaAccountInfoV3, ReplicaBlockInfoV3, ReplicaTransactionInfoV2, SlotStatus
    }, 
    solana_measure::measure::Measure, solana_metrics::*, 
    solana_runtime::bank::RewardType,
    solana_sdk::{address_lookup_table::instruction, instruction::{CompiledInstruction, Instruction}, message::{v0::{self, LoadedAddresses, MessageAddressTableLookup}, 
    Message,MessageHeader,SanitizedMessage}, pubkey, timing::AtomicInterval, transaction::TransactionError}, 
    solana_transaction_status::{InnerInstructions, Reward, TransactionStatus, TransactionStatusMeta,TransactionTokenBalance}, 
    std::{
        collections::HashSet, result, sync::{
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
            Arc, Mutex,
        }, thread::{self, sleep, Builder, JoinHandle}, time::Duration, {path::PathBuf, any::Any}
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

pub struct LogTransactionRequest {
    pub transaction_info: DbTransaction,
}

impl From<&MessageAddressTableLookup> for DbTransactionMessageAddressTableLookup {
    fn from(address_table_lookup: &MessageAddressTableLookup) -> Self {
        Self {
            account_key: address_table_lookup.account_key.as_ref().to_vec(),
            writable_indexes: address_table_lookup
                .writable_indexes
                .iter()
                .map(|idx| *idx as i16)
                .collect(),
            readonly_indexes: address_table_lookup
                .readonly_indexes
                .iter()
                .map(|idx| *idx as i16)
                .collect(),
        }
    }
}

impl From <&LoadedAddresses> for DbLoadedAddresses{
    fn from(loaded_addresses: &LoadedAddresses) -> Self {
        Self{
            writable: loaded_addresses.writable.iter()
            .map(|pubkey| pubkey.as_ref().to_vec())
            .collect(),
            readonly: loaded_addresses.readonly.iter().
            map(|pubkey|pubkey.as_ref().to_vec())
            .collect()
        }
    }
}

impl From<&MessageHeader> for DbTransactionMessageHeader{
    fn from(message_header: &MessageHeader) -> Self {
        Self{
            num_required_signatures:message_header.num_required_signatures as i16,
            num_readonly_signed_accounts:message_header.num_readonly_signed_accounts as i16,
            num_readonly_unsigned_accounts:message_header.num_readonly_unsigned_accounts as i16
        }
    }
}

impl From<&CompiledInstruction> for DbCompiledInstruction {
    fn from(compiled_instruction: &CompiledInstruction) -> Self {
        Self { program_id_index: compiled_instruction.program_id_index as i16, 
            accounts: compiled_instruction.accounts
            .iter().map(|account_idx| *account_idx as i16).
            collect(), 
            data: compiled_instruction.data.clone()
        }
    }
    
}

impl From<&Message> for DbTransactionMessage{
    fn from(message: &Message) -> Self {
        Self{
            header:DbTransactionMessageHeader::from(&message.header),
            account_keys:message.account_keys.iter().map(|key|key.as_ref().to_vec()).collect(),
            recent_blockhash:message.recent_blockhash.as_ref().to_vec(),
            instructions:message
            .instructions
            .iter()
            .map(DbCompiledInstruction::from)
            .collect(),
        }
    }
}

impl From<&v0::Message> for DbTransactionMessageV0 {
    fn from(message: &v0::Message) -> Self {
        Self {
            header: DbTransactionMessageHeader::from(&message.header),
            account_keys: message
                .account_keys
                .iter()
                .map(|key| key.as_ref().to_vec())
                .collect(),
            recent_blockhash: message.recent_blockhash.as_ref().to_vec(),
            instructions: message
                .instructions
                .iter()
                .map(DbCompiledInstruction::from)
                .collect(),
            address_table_lookups: message
                .address_table_lookups
                .iter()
                .map(DbTransactionMessageAddressTableLookup::from)
                .collect(),
        }
    }
}

impl<'a> From<&v0::LoadedMessage<'a>> for DbLoadedMessageV0 {
    fn from(message: &v0::LoadedMessage) -> Self {
        Self {
            message: DbTransactionMessageV0::from(&message.message as &v0::Message),
            loaded_addresses: DbLoadedAddresses::from(
                &message.loaded_addresses as &LoadedAddresses,
            ),
        }
    }
}


impl From<&InnerInstructions> for DbInnerInstructions {
    fn from(instructions: &InnerInstructions) -> Self {
        Self {
            index: instructions.index as i16,
            instructions: instructions
                .instructions
                .iter()
                .map(|instruction| DbCompiledInstruction::from(&instruction.instruction))
                .collect(),
        }
    }
}

impl From<&RewardType> for DbRewardType {
    fn from(reward_type: &RewardType) -> Self {
        match reward_type {
            RewardType::Fee => Self::Fee,
            RewardType::Rent => Self::Rent,
            RewardType::Staking => Self::Staking,
            RewardType::Voting => Self::Voting,
        }
    }
}

fn get_reward_type(reward:&Option<RewardType>)->Option<DbRewardType>{
    reward.as_ref().map(DbRewardType::from)
}

impl From<&Reward> for DbReward {
    fn from(reward: &Reward) -> Self {
        Self {
            pubkey: reward.pubkey.clone(),
            lamports: reward.lamports,
            post_balance: reward.post_balance as i64,
            reward_type: get_reward_type(&reward.reward_type),
            commission: reward
                .commission
                .as_ref()
                .map(|commission| *commission as i16),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbTransactionErrorCode {
    AccountInUse,
    AccountLoadedTwice,
    AccountNotFound,
    ProgramAccountNotFound,
    InsufficientFundsForFee,
    InvalidAccountForFee,
    AlreadyProcessed,
    BlockhashNotFound,
    InstructionError,
    CallChainTooDeep,
    MissingSignatureForFee,
    InvalidAccountIndex,
    SignatureFailure,
    InvalidProgramForExecution,
    SanitizeFailure,
    ClusterMaintenance,
    AccountBorrowOutstanding,
    WouldExceedMaxAccountCostLimit,
    WouldExceedMaxBlockCostLimit,
    UnsupportedVersion,
    InvalidWritableAccount,
    WouldExceedMaxAccountDataCostLimit,
    TooManyAccountLocks,
    AddressLookupTableNotFound,
    InvalidAddressLookupTableOwner,
    InvalidAddressLookupTableData,
    InvalidAddressLookupTableIndex,
    InvalidRentPayingAccount,
    WouldExceedMaxVoteCostLimit,
    WouldExceedAccountDataBlockLimit,
    WouldExceedAccountDataTotalLimit,
    DuplicateInstruction,
    InsufficientFundsForRent,
    MaxLoadedAccountsDataSizeExceeded,
    InvalidLoadedAccountsDataSizeLimit,
    ResanitizationNeeded,
    UnbalancedTransaction,
    ProgramExecutionTemporarilyRestricted,
    Other(String)
}

impl From<&TransactionError> for DbTransactionErrorCode {
    fn from(err: &TransactionError) -> Self {
        match err {
            TransactionError::AccountInUse => Self::AccountInUse,
            TransactionError::AccountLoadedTwice => Self::AccountLoadedTwice,
            TransactionError::AccountNotFound => Self::AccountNotFound,
            TransactionError::ProgramAccountNotFound => Self::ProgramAccountNotFound,
            TransactionError::InsufficientFundsForFee => Self::InsufficientFundsForFee,
            TransactionError::InvalidAccountForFee => Self::InvalidAccountForFee,
            TransactionError::AlreadyProcessed => Self::AlreadyProcessed,
            TransactionError::BlockhashNotFound => Self::BlockhashNotFound,
            TransactionError::InstructionError(_idx, _error) => Self::InstructionError,
            TransactionError::CallChainTooDeep => Self::CallChainTooDeep,
            TransactionError::MissingSignatureForFee => Self::MissingSignatureForFee,
            TransactionError::InvalidAccountIndex => Self::InvalidAccountIndex,
            TransactionError::SignatureFailure => Self::SignatureFailure,
            TransactionError::InvalidProgramForExecution => Self::InvalidProgramForExecution,
            TransactionError::SanitizeFailure => Self::SanitizeFailure,
            TransactionError::ClusterMaintenance => Self::ClusterMaintenance,
            TransactionError::AccountBorrowOutstanding => Self::AccountBorrowOutstanding,
            TransactionError::WouldExceedMaxAccountCostLimit => {
                Self::WouldExceedMaxAccountCostLimit
            }
            TransactionError::WouldExceedMaxBlockCostLimit => Self::WouldExceedMaxBlockCostLimit,
            TransactionError::UnsupportedVersion => Self::UnsupportedVersion,
            TransactionError::InvalidWritableAccount => Self::InvalidWritableAccount,
            TransactionError::WouldExceedAccountDataBlockLimit => {
                Self::WouldExceedAccountDataBlockLimit
            }
            TransactionError::WouldExceedAccountDataTotalLimit => {
                Self::WouldExceedAccountDataTotalLimit
            }
            TransactionError::TooManyAccountLocks => Self::TooManyAccountLocks,
            TransactionError::AddressLookupTableNotFound => Self::AddressLookupTableNotFound,
            TransactionError::InvalidAddressLookupTableOwner => {
                Self::InvalidAddressLookupTableOwner
            }
            TransactionError::InvalidAddressLookupTableData => Self::InvalidAddressLookupTableData,
            TransactionError::InvalidAddressLookupTableIndex => {
                Self::InvalidAddressLookupTableIndex
            }
            TransactionError::InvalidRentPayingAccount => Self::InvalidRentPayingAccount,
            TransactionError::WouldExceedMaxVoteCostLimit => Self::WouldExceedMaxVoteCostLimit,
            TransactionError::DuplicateInstruction(_) => Self::DuplicateInstruction,
            TransactionError::InsufficientFundsForRent { account_index: _ } => {
                Self::InsufficientFundsForRent
            }
            TransactionError::MaxLoadedAccountsDataSizeExceeded => {
                Self::MaxLoadedAccountsDataSizeExceeded
            }
            TransactionError::InvalidLoadedAccountsDataSizeLimit => {
                Self::InvalidLoadedAccountsDataSizeLimit
            }
            TransactionError::ResanitizationNeeded => Self::ResanitizationNeeded,
            TransactionError::UnbalancedTransaction => Self::UnbalancedTransaction,
            TransactionError::ProgramExecutionTemporarilyRestricted { account_index: _ } => {
                Self::ProgramExecutionTemporarilyRestricted
            }
        }
        
    }
}

#[derive(Clone, Debug, Eq, Serialize,Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct DbTransactionError {
    error_code: DbTransactionErrorCode,
    error_detail: Option<String>,
}

fn get_transaction_error(result: &Result<(), TransactionError>) -> Option<DbTransactionError> {
    if result.is_ok() {
        return None;
    }

    let error = result.as_ref().err().unwrap();
    Some(DbTransactionError {
        error_code: DbTransactionErrorCode::from(error),
        error_detail: {
            if let TransactionError::InstructionError(idx, instruction_error) = error {
                let mut error_detail = format!(
                    "InstructionError: idx ({}), error: ({})",
                    idx, instruction_error
                );
                if error_detail.len() > MAX_TRANSACTION_STATUS_LEN {
                    error_detail = error_detail
                        .to_string()
                        .split_off(MAX_TRANSACTION_STATUS_LEN);
                }
                Some(error_detail)
            } else {
                None
            }
        },
    })
}
//Sample error format
// {
//     "error_code": "instruction_error",
//     "details": {
//         "instruction_index": 2,
//         "error_code": "InvalidArgument"
//     }
// }

impl From<&TransactionTokenBalance> for DbTransactionTokenBalance {
    fn from(token_balance: &TransactionTokenBalance) -> Self {
        Self {
            account_index: token_balance.account_index as i16,
            mint: token_balance.mint.clone(),
            ui_token_amount: token_balance.ui_token_amount.ui_amount,
            owner: token_balance.owner.clone(),
        }
    }
}

impl From <&TransactionStatusMeta> for DbTransactionStatusMeta{
    fn from(meta: &TransactionStatusMeta) -> Self {
        Self{
            error: get_transaction_error(&meta.status),
            fee:meta.fee as i64,
            pre_balances:meta.pre_balances.iter().map(|balance| *balance as i64).collect(),
            post_balances:meta.post_balances.iter().map(|balance| *balance as i64).collect(),
            inner_instructions:meta.inner_instructions.as_ref().map(|instructions| instructions.iter().map(DbInnerInstructions::from).collect()),
            log_messages:meta.log_messages.clone(),
            pre_token_balances:meta.pre_token_balances.as_ref().map(|balances| {balances
                .iter()
                .map(DbTransactionTokenBalance::from)
                .collect()}),
            post_token_balances:meta.post_token_balances.as_ref().map(|balances| {balances
                .iter()
                .map(DbTransactionTokenBalance::from)
                .collect()}),
            rewards:meta.rewards.as_ref().map(|rewards| {rewards
                .iter()
                .map(DbReward::from)
                .collect()})
        }
    }
}

//constructs the transaction database
fn build_db_transaction(
    slot: u64,
    transaction_info: &ReplicaTransactionInfoV2,
    transaction_write_version: u64,
) -> DbTransaction {
    DbTransaction {
        signature: transaction_info.signature.as_ref().to_vec(),
        is_vote: transaction_info.is_vote,
        slot: slot as i64,
        message_type: match transaction_info.transaction.message() {
            SanitizedMessage::Legacy(_) => 0,
            SanitizedMessage::V0(_) => 1,
        },
        legacy_message: match transaction_info.transaction.message() {
            SanitizedMessage::Legacy(legacy_message) => {
                Some(DbTransactionMessage::from(legacy_message.message.as_ref()))
            }
            _ => None,
        },
        v0_loaded_message: match transaction_info.transaction.message() {
            SanitizedMessage::V0(loaded_message) => Some(DbLoadedMessageV0::from(loaded_message)),
            _ => None,
        },
        signatures: transaction_info
            .transaction
            .signatures()
            .iter()
            .map(|signature| signature.as_ref().to_vec())
            .collect(),
        message_hash: transaction_info
            .transaction
            .message_hash()
            .as_ref()
            .to_vec(),
        meta: DbTransactionStatusMeta::from(transaction_info.transaction_status_meta),
        write_version: transaction_write_version as i64,
        index: transaction_info.index as i64,
    }
}


//MongoDB_CLIENT

///Wraps MongoDB client connection and prepared statements
struct MongodbClientWrapper {
    client: mongodb::Client,
    accounts_collection:mongodb::Collection<DbAccountInfo>,
    slots_collection:mongodb::Collection<SlotMetadata>,
    transactions_colection:mongodb::Collection<TransactionLog>,
    token_owner_index_collection: Option<mongodb::Collection<TokenSecondaryIndexEntry>>,
    token_mint_index_collection: Option<mongodb::Collection<TokenSecondaryIndexEntry>>,
}

///Handles pending updates, config options, index management
pub struct SimpleMongoDbClient {
    batch_size: usize,
    slots_at_startup: HashSet<u64>, //Hashset may consume sig memory if many slots are processed at startup. consider using a bitmap/Roaringbitmap here if slots are relatively dense?
    pending_account_updates: Vec<DbAccountInfo>,
    index_token_owner: bool,
    index_token_mint: bool,
    pending_token_owner_index: Vec<TokenSecondaryIndexEntry>,
    pending_token_mint_index: Vec<TokenSecondaryIndexEntry>,
    client: tokio::sync::Mutex<MongodbClientWrapper>, //allow thread-safe access to client wrapper
}

///Defines worker logic ad tracks startup state
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
    async fn connect_to_db(config: &GeyserPluginMongoDBConfig)->Result<Client, GeyserPluginError>{
        let port=config.port.unwrap_or(DEFAULT_MONGO_DB_PORT);
        let connection_str= if let Some(connection_str)= &config.connection_str{
            connection_str.clone()
        }else{
            if config.host.is_none() || config.user.is_none(){
                let msg = format!(
                    "\"connection_str\": {:?}, or \"host\": {:?} \"user\": {:?} must be specified",
                    config.connection_str, config.host, config.user
                );
                return Err(GeyserPluginError::Custom(Box::new(
                    GeyserPluginMongoDbError::ConfigurationError { msg },

                )))
            }
            format!(
                "host={} user={} port={}",
                config.host.as_ref().unwrap(),
                config.user.as_ref().unwrap(),
                port
            )
        };
        // Configure MongoDB client options
        let mut client_options = ClientOptions::parse(&connection_str).await.map_err(|err| {
        GeyserPluginMongoDbError::ConfigurationError {
            msg: format!("Invalid connection string: {}. Error: {}", connection_str, err),
        }
    })?;

          // Configure TLS if use_ssl is enabled
    if let Some(true) = config.use_ssl {
        if config.server_ca.is_none() || config.client_cert.is_none() || config.client_key.is_none() {
            let missing_fields: Vec<&str> = [
                ("server_ca", config.server_ca.is_none()),
                ("client_cert", config.client_cert.is_none()),
                ("client_key", config.client_key.is_none()),
            ]
            .iter()
            .filter(|(_, missing)| *missing)
            .map(|(field, _)| *field)
            .collect();

            let msg = format!(
                "{} must be specified when \"use_ssl\" is set.",
                missing_fields.join(", ")
            );
            return Err(GeyserPluginError::Custom(Box::new(
                GeyserPluginMongoDbError::ConfigurationError { msg },
            )));
        }
    
        let server_ca = config.server_ca.as_ref().map(PathBuf::from); 
        //     // let client_cert = config.client_cert.as_ref().map(PathBuf::from);  
            let client_key = config.client_key.as_ref().map(PathBuf::from); 
        
      // Set up TLS options
      let tls_options = TlsOptions::builder()
      .ca_file_path(server_ca)
      .cert_key_file_path(client_key)
      .build();

  client_options.tls = Some(Tls::Enabled(tls_options));
}
 // Create the MongoDB client
 match Client::with_options(client_options) {
    Ok(client) => Ok(client),
    Err(err) => {
        let msg = format!(
            "Error in connecting to the MongoDB database: {:?} connection_str: {:?}",
            err, connection_str
        );
        Err(GeyserPluginError::Custom(Box::new(
            GeyserPluginMongoDbError::DataStoreConnectionError { msg },
        )))
    }
}
}
// Create the MongoDB client
fn build_bulk_account_insert_statement()->Result<Statement, GeyserPluginError>{

}
}




















}
// impl SimpleMongoDbClient {
    
// async fn parse_and_validate_config(
//     config: &GeyserPluginMongoDBConfig,
// ) -> Result<ClientOptions, GeyserPluginMongoDbError> {
//     let connection_str = if let Some(connection_str) = &config.connection_str {
//         connection_str.clone()
//     } else {
//         let host = config.host.as_ref().ok_or_else(|| {
//             GeyserPluginMongoDbError::ConfigurationError {
//                 msg: "Missing 'host' configuration".to_string(),
//             }
//         })?;
//         let user = config.user.as_ref().ok_or_else(|| {
//             GeyserPluginMongoDbError::ConfigurationError {
//                 msg: "Missing 'user' configuration".to_string(),
//             }
//         })?;
//         let port = config.port.unwrap_or(DEFAULT_MONGO_DB_PORT);
//         // let password = config.password.as_deref().unwrap_or("");
//         format!("mongodb://{}:@{}:{}", user, host, port)
//     };

//     let mut client_options = ClientOptions::parse(&connection_str).await.map_err(|err| {
//         GeyserPluginMongoDbError::ConfigurationError {
//             msg: format!("Invalid connection string: {}. Error: {}", connection_str, err),
//         }
//     })?;

//     if config.use_ssl.unwrap_or(false) {
//         let tls_options = Self::configure_tls(config)?;
//         client_options.tls = Some(Tls::Enabled(tls_options));
//     }

//     Ok(client_options)
// }

// fn configure_tls(config: &GeyserPluginMongoDBConfig) -> Result<TlsOptions, GeyserPluginMongoDbError> {
//     let server_ca = config.server_ca.as_ref().map(PathBuf::from); 
//     // let client_cert = config.client_cert.as_ref().map(PathBuf::from);  
//     let client_key = config.client_key.as_ref().map(PathBuf::from); 

//     TlsOptions::builder()
//     .ca_file_path(server_ca)
//     .cert_key_file_path(client_key) // Corrected method and single argument
//     .build();
//     Ok(())

// }
// }
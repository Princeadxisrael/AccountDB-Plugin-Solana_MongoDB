# AccountDB-Plugin-Solana_MongoDB
The Geyser plugin is an optimized mechanism for streaming real-time transaction, block, and account data from the Solana runtime into external data systems to bypass RPC servers, reducing latency and ensuring high-throughput data delivery.Geyser plugins allow developers/infra providers to stream real-time data such as accounts(state), blocks, slots, and transactions.Given that Solana is an eventually consistent poorly indexed database with complicated write semantics that aims at high throughput read and write on multiple transaction isolation levels, GP extends Solana’s functionality by enabling applications to consume on-chain data directly instead of repeatedly querying rpc nodes.

# Purpose
Build a Solana Geyser plugin with MongoDB to integrate Solana's real-time data streaming capabilities with MongoDB's flexible document-oriented database and perform benchmark tests against existing solutions.

### Account Selection

The `accounts_selector` can be used to filter the accounts that should be persisted.

For example, one can use the following to persist only the accounts with particular
Base58-encoded Pubkeys,

```
    "accounts_selector" : {
         "accounts" : ["pubkey-1", "pubkey-2", ..., "pubkey-n"],
    }
```

Or use the following to select accounts with certain program owners:

```
    "accounts_selector" : {
         "owners" : ["pubkey-owner-1", "pubkey-owner-2", ..., "pubkey-owner-m"],
    }
```

To select all accounts, use the wildcard character (*):

```
    "accounts_selector" : {
         "accounts" : ["*"],
    }
```

### Transaction Selection

`transaction_selector`, controls if and what transactions to store.
If this field is missing, none of the transactions are stored.

For example, one can use the following to select only the transactions
referencing accounts with particular Base58-encoded Pubkeys,

```
"transaction_selector" : {
    "mentions" : \["pubkey-1", "pubkey-2", ..., "pubkey-n"\],
}
```

The `mentions` field supports wildcards to select all transaction or
all 'vote' transactions. For example, to select all transactions:

```
"transaction_selector" : {
    "mentions" : \["*"\],
}
```

To select all vote transactions:

```
"transaction_selector" : {
    "mentions" : \["all_votes"\],
}
```

# Data Model Designs?
| Collection         | Description             |
|:--------------|:------------------------|
| transaction   | transaction data        |
| block         | Block metadata          |
| slot          | Slot metadata           |
| account       | account data            |
| account_audit | Account historical data |


- Transactions -> `transaction` collection
- Accounts    ->  `accounts` collection
- Blocks      ->  `blocks` collection
- Slots       ->  `slots` collection

# Indexing for Query 
Indexing by transaction signature
indexing by public keys

# Data Processing
## filter data:
Data to be streamed and stored:
- Transactions
- Accounts
- Block data
- Slots

## Data transformation
-Serialize binary data to BSON 
-Include metadata for efficient queries

## Optimization
- ### batch writes: buffer updates and batch write them
- ### connection between the plugin interface and MongoDB (conncetion pool)
- ###  event blocking: Avoid event blocking
- ### Data retention: implement TTL or periodic pruning to manage storage
- ### Achieve snapshot isolation and consistency through MongoDB transactions, sharding, columnar compression, densification, deletes, and gap-filling for time series collections.
- ### Benchmark test its read performance against existing implementations.
- ### Benchmark test its write performance against existing implementations.






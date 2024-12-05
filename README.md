# Custom-Geyser-Plugin-Solana
The Geyser plugin is an optimized mechanism for streaming real-time transaction, block, and account data from the Solana runtime into external data systems to bypass RPC servers, reducing latency and ensuring high-throughput data delivery.Geyser plugins allow developers/infra providers to stream real-time data such as accounts(state), blocks, slots, and transactions.Given that Solana is an eventually consistent poorly indexed database with complicated write semantics that aims at high throughput read and write on multiple transaction isolation levels, GP extends Solana’s functionality by enabling applications to consume on-chain data directly instead of repeatedly querying rpc nodes.

# Purpose
Build a Solana Geyser plugin with MongoDB to integrate Solana's real-time data streaming capabilities with MongoDB's flexible document-oriented database and perform benchmark tests against existing solutions.

# Data Model Designs?
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






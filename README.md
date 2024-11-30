# Custom-Geyser-Plugin-Solana
Geyser plugins allow developers/infra providers to stream real-time data such as accounts(state), blocks, slots, and transactions. Solana is an eventually consistent poorly indexed database with complicated write semantics that aims at high throughput read and write on multiple transaction isolation levels. GP extends Solana’s functions by enabling applications to consume on-chain data directly instead of repeatedly querying rpc nodes.

# Purpose
Implement and benchmark test the geyser plugin implementation with MongoDB as its distributed database

Who are Users? 

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
-Transactions
-Accounts

## Data transformation
-Serialize binary data to JSON
-Include metadata for efficient queries

## Optimization


## Architecture Diagrams of Accounts DB
find inside ArchDiagrams floder


- ### batch writes: buffer updates and batch write them
- ### connection between the plugin interface and MongoDB (conncetion pool)
- ###  event blocking: Avoid event blocking
- ### Data retention: implement TTL or periodic pruning to manage storage


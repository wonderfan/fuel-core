use crate::{
    fuel_core_graphql_api::storage::{
        blocks::FuelBlockIdsToHeights,
        coins::OwnedCoins,
        messages::OwnedMessageIds,
        transactions::{
            OwnedTransactionIndexKey,
            OwnedTransactions,
            TransactionStatuses,
        },
    },
    graphql_api::ports::worker::OffChainDatabaseTransaction,
};
use fuel_core_storage::{
    kv_store::{
        KeyValueInspect,
        StorageColumn,
    },
    transactional::{
        Modifiable,
        StorageTransaction,
    },
    Error as StorageError,
    Result as StorageResult,
    StorageAsMut,
    StorageAsRef,
    StorageMutate,
};
use fuel_core_types::{
    fuel_tx::{
        Address,
        Bytes32,
    },
    fuel_types::BlockHeight,
    services::txpool::TransactionExecutionStatus,
};
use statistic::StatisticTable;

pub mod assets;
pub mod balances;
pub mod blocks;
pub mod coins;
pub mod contracts;
pub mod da_compression;
pub mod messages;
pub mod old;
pub mod statistic;
pub mod transactions;

pub mod relayed_transactions;
/// Tracks the total number of transactions written to the chain
/// It's useful for analyzing TPS or other metrics.
const TX_COUNT: &str = "total_tx_count";

/// GraphQL database tables column ids to the corresponding [`fuel_core_storage::Mappable`] table.
#[repr(u32)]
#[derive(
    Copy,
    Clone,
    Debug,
    strum_macros::EnumCount,
    strum_macros::IntoStaticStr,
    PartialEq,
    Eq,
    enum_iterator::Sequence,
    Hash,
)]
pub enum Column {
    /// The column id of metadata about the blockchain
    Metadata = 0,
    /// Metadata for genesis progress
    GenesisMetadata = 1,
    /// The column of the table that stores `true` if `owner` owns `Coin` with `coin_id`
    OwnedCoins = 2,
    /// Transaction id to current status
    TransactionStatus = 3,
    /// The column of the table of all `owner`'s transactions
    TransactionsByOwnerBlockIdx = 4,
    /// The column of the table that stores `true` if `owner` owns `Message` with `message_id`
    OwnedMessageIds = 5,
    /// The column of the table that stores statistic about the blockchain.
    Statistic = 6,
    /// See [`blocks::FuelBlockIdsToHeights`]
    FuelBlockIdsToHeights = 7,
    /// See [`ContractsInfo`](contracts::ContractsInfo)
    ContractsInfo = 8,
    /// See [`OldFuelBlocks`](old::OldFuelBlocks)
    OldFuelBlocks = 9,
    /// See [`OldFuelBlockConsensus`](old::OldFuelBlockConsensus)
    OldFuelBlockConsensus = 10,
    /// See [`OldTransactions`](old::OldTransactions)
    OldTransactions = 11,
    /// Relayed Tx ID to Layer 1 Relayed Transaction status
    RelayedTransactionStatus = 12,
    /// Messages that have been spent.
    /// Existence of a key in this column means that the message has been spent.
    /// See [`SpentMessages`](messages::SpentMessages)
    SpentMessages = 13,
    /// DA compression and postcard serialized blocks.
    /// See [`DaCompressedBlocks`](da_compression::DaCompressedBlocks)
    DaCompressedBlocks = 14,
    /// See [`DaCompressionTemporalRegistryIndex`](da_compression::DaCompressionTemporalRegistryIndex)
    DaCompressionTemporalRegistryIndex = 15,
    /// See [`DaCompressionTemporalRegistryTimestamps`](da_compression::DaCompressionTemporalRegistryTimestamps)
    DaCompressionTemporalRegistryTimestamps = 16,
    /// See [`DaCompressionTemporalRegistryEvictorCache`](da_compression::DaCompressionTemporalRegistryEvictorCache)
    DaCompressionTemporalRegistryEvictorCache = 17,
    /// See [`DaCompressionTemporalRegistryAddress`](da_compression::DaCompressionTemporalRegistryAddress)
    DaCompressionTemporalRegistryAddress = 18,
    /// See [`DaCompressionTemporalRegistryAssetId`](da_compression::DaCompressionTemporalRegistryAssetId)
    DaCompressionTemporalRegistryAssetId = 19,
    /// See [`DaCompressionTemporalRegistryContractId`](da_compression::DaCompressionTemporalRegistryContractId)
    DaCompressionTemporalRegistryContractId = 20,
    /// See [`DaCompressionTemporalRegistryScriptCode`](da_compression::DaCompressionTemporalRegistryScriptCode)
    DaCompressionTemporalRegistryScriptCode = 21,
    /// See [`DaCompressionTemporalRegistryPredicateCode`](da_compression::DaCompressionTemporalRegistryPredicateCode)
    DaCompressionTemporalRegistryPredicateCode = 22,
    /// Coin balances per account and asset.
    CoinBalances = 23,
    /// Message balances per account.
    MessageBalances = 24,
    /// See [`AssetsInfo`](assets::AssetsInfo)
    AssetsInfo = 25,
    /// Index of the coins that are available to spend.
    CoinsToSpend = 26,
    /// See [`DaCompressionTemporalRegistryAddressV2`](da_compression::v2::address::DaCompressionTemporalRegistryAddressV2)
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryAddressV2 = 27,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalAddressMerkleData = 28,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalAddressMerkleMetadata = 29,
    // See [`DaCompressionTemporalRegistryAssetIdV2`](da_compression::v2::asset_id::DaCompressionTemporalRegistryAssetIdV2)
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryAssetIdV2 = 30,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalAssetIdMerkleData = 31,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalAssetIdMerkleMetadata = 32,
    /// See [`DaCompressionTemporalRegistryContractIdV2`](da_compression::v2::contract_id::DaCompressionTemporalRegistryContractIdV2)
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryContractIdV2 = 33,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalContractIdMerkleData = 34,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalContractIdMerkleMetadata = 35,
    /// See [`DaCompressionTemporalRegistryScriptCodeV2`](da_compression::v2::script_code::DaCompressionTemporalRegistryScriptCodeV2)
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryScriptCodeV2 = 36,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalScriptCodeMerkleData = 37,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalScriptCodeMerkleMetadata = 38,
    /// See [`DaCompressionTemporalRegistryPredicateCodeV2`](da_compression::v2::predicate_code::DaCompressionTemporalRegistryPredicateCodeV2)
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryPredicateCodeV2 = 39,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalPredicateCodeMerkleData = 40,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalPredicateCodeMerkleMetadata = 41,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryIndexV2 = 42,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryIndexMerkleData = 43,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryIndexMerkleMetadata = 44,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryTimestampsV2 = 45,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryTimestampsMerkleData = 46,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryTimestampsMerkleMetadata = 47,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryEvictorCacheV2 = 48,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryEvictorCacheMerkleData = 49,
    #[cfg(feature = "fault-proving")]
    DaCompressionTemporalRegistryEvictorCacheMerkleMetadata = 50,
}

impl Column {
    /// The total count of variants in the enum.
    pub const COUNT: usize = <Self as strum::EnumCount>::COUNT;

    /// Returns the `usize` representation of the `Column`.
    pub fn as_u32(&self) -> u32 {
        *self as u32
    }
}

impl StorageColumn for Column {
    fn name(&self) -> String {
        let str: &str = self.into();
        str.to_string()
    }

    fn id(&self) -> u32 {
        self.as_u32()
    }
}

impl<S> OffChainDatabaseTransaction for StorageTransaction<S>
where
    S: KeyValueInspect<Column = Column> + Modifiable,
    StorageTransaction<S>: StorageMutate<OwnedMessageIds, Error = StorageError>
        + StorageMutate<OwnedCoins, Error = StorageError>
        + StorageMutate<FuelBlockIdsToHeights, Error = StorageError>,
{
    fn record_tx_id_owner(
        &mut self,
        owner: &Address,
        block_height: BlockHeight,
        tx_idx: u16,
        tx_id: &Bytes32,
    ) -> StorageResult<()> {
        self.storage::<OwnedTransactions>().insert(
            &OwnedTransactionIndexKey::new(owner, block_height, tx_idx),
            tx_id,
        )
    }

    fn update_tx_status(
        &mut self,
        id: &Bytes32,
        status: TransactionExecutionStatus,
    ) -> StorageResult<Option<TransactionExecutionStatus>> {
        self.storage::<TransactionStatuses>().replace(id, &status)
    }

    fn increase_tx_count(&mut self, new_txs_count: u64) -> StorageResult<u64> {
        // TODO: how should tx count be initialized after regenesis?
        let current_tx_count: u64 = self.get_tx_count()?;
        // Using saturating_add because this value doesn't significantly impact the correctness of execution.
        let new_tx_count = current_tx_count.saturating_add(new_txs_count);
        <_ as StorageMutate<StatisticTable<u64>>>::insert(self, TX_COUNT, &new_tx_count)?;
        Ok(new_tx_count)
    }

    fn get_tx_count(&self) -> StorageResult<u64> {
        let tx_count = self
            .storage::<StatisticTable<u64>>()
            .get(TX_COUNT)?
            .unwrap_or_default()
            .into_owned();
        Ok(tx_count)
    }

    fn commit(self) -> StorageResult<()> {
        self.commit()?;
        Ok(())
    }
}

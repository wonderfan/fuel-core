use crate::{
    column::Column,
    Coins,
    ConsensusParametersVersions,
    ContractsLatestUtxo,
    Messages,
    StateTransitionBytecodeVersions,
};
use alloc::{
    borrow::Cow,
    vec,
    vec::Vec,
};
use fuel_core_storage::{
    iter::IterableStore,
    kv_store::KeyValueInspect,
    transactional::StorageTransaction,
    StorageAsMut,
};
use fuel_core_types::{
    blockchain::{
        block::Block,
        header::{
            ConsensusParametersVersion,
            StateTransitionBytecodeVersion,
        },
    },
    entities::{
        coins::coin::CompressedCoinV1,
        contract::ContractUtxoInfo,
    },
    fuel_asm::Word,
    fuel_tx::{
        field::{
            InputContract,
            Inputs,
            OutputContract,
            Outputs,
            UpgradePurpose as _,
        },
        input::{
            self,
            coin::{
                CoinPredicate,
                CoinSigned,
            },
            message::{
                MessageCoinPredicate,
                MessageCoinSigned,
                MessageDataPredicate,
                MessageDataSigned,
            },
        },
        output::{
            self,
        },
        Address,
        AssetId,
        ChargeableTransaction,
        Input,
        Output,
        Transaction,
        TxPointer,
        UniqueIdentifier,
        UpgradeBody,
        UpgradeMetadata,
        UpgradePurpose,
        UtxoId,
    },
    fuel_types::{
        BlockHeight,
        ChainId,
    },
    services::executor::{
        Error as ExecutorError,
        TransactionValidityError,
    },
};

pub trait UpdateMerklizedTables {
    fn update_merklized_tables(
        &mut self,
        chain_id: ChainId,
        block: &Block,
        latest_state_transition_bytecode_version: StateTransitionBytecodeVersion,
        latest_consensus_parameters_version: ConsensusParametersVersion,
    ) -> anyhow::Result<()>;
}

impl<Storage> UpdateMerklizedTables for StorageTransaction<Storage>
where
    Storage: KeyValueInspect<Column = Column> + IterableStore,
{
    fn update_merklized_tables(
        &mut self,
        chain_id: ChainId,
        block: &Block,
        latest_state_transition_bytecode_version: StateTransitionBytecodeVersion,
        latest_consensus_parameters_version: ConsensusParametersVersion,
    ) -> anyhow::Result<()> {
        let mut update_transaction = UpdateMerklizedTablesTransaction {
            chain_id,
            storage: self,
            latest_state_transition_bytecode_version,
            latest_consensus_parameters_version,
        };

        update_transaction.process_block(block)
    }
}

struct UpdateMerklizedTablesTransaction<'a, Storage> {
    chain_id: ChainId,
    storage: &'a mut StorageTransaction<Storage>,
    latest_consensus_parameters_version: ConsensusParametersVersion,
    latest_state_transition_bytecode_version: StateTransitionBytecodeVersion,
}

impl<'a, Storage> UpdateMerklizedTablesTransaction<'a, Storage>
where
    Storage: KeyValueInspect<Column = Column> + IterableStore,
{
    // TODO(#2588): Proper result type
    pub fn process_block(&mut self, block: &Block) -> anyhow::Result<()> {
        let block_height = *block.header().height();

        for (tx_idx, tx) in block.transactions().iter().enumerate() {
            let tx_idx: u16 =
                u16::try_from(tx_idx).map_err(|_| ExecutorError::TooManyTransactions)?;
            self.process_transaction(block_height, tx_idx, tx)?;
        }

        Ok(())
    }

    fn process_transaction(
        &mut self,
        block_height: BlockHeight,
        tx_idx: u16,
        tx: &Transaction,
    ) -> anyhow::Result<()> {
        let inputs = tx.inputs();
        for input in inputs.iter() {
            self.process_input(input)?;
        }

        let tx_pointer = TxPointer::new(block_height, tx_idx);
        for (output_index, output) in tx.outputs().iter().enumerate() {
            let output_index =
                u16::try_from(output_index).map_err(|_| ExecutorError::TooManyOutputs)?;

            let tx_id = tx.id(&self.chain_id);
            let utxo_id = UtxoId::new(tx_id, output_index);
            self.process_output(tx_pointer, utxo_id, &inputs, output)?;
        }

        match tx {
            Transaction::Upgrade(tx) => {
                self.process_upgrade_transaction(tx)?;
            }
            _ => {}
        }
        // TODO(#2583): Add the transaction to the `ProcessedTransactions` table.
        // TODO(#2584): Insert state transition bytecode and consensus parameter updates.
        // TODO(#2585): Insert uplodade bytecodes.
        // TODO(#2586): Insert blobs.
        // TODO(#2587): Insert raw code for created contracts.

        Ok(())
    }

    fn process_input(&mut self, input: &Input) -> anyhow::Result<()> {
        match input {
            Input::CoinSigned(CoinSigned { utxo_id, .. })
            | Input::CoinPredicate(CoinPredicate { utxo_id, .. }) => {
                self.storage.storage_as_mut::<Coins>().remove(utxo_id)?;
            }
            Input::Contract(_) => {
                // Do nothing, since we are interested in output values
            }
            Input::MessageCoinSigned(MessageCoinSigned { nonce, .. })
            | Input::MessageCoinPredicate(MessageCoinPredicate { nonce, .. }) => {
                self.storage.storage_as_mut::<Messages>().remove(nonce)?;
            }
            // The messages below are retryable, it means that if execution failed,
            // message is not spend.
            Input::MessageDataSigned(MessageDataSigned { nonce, .. })
            | Input::MessageDataPredicate(MessageDataPredicate { nonce, .. }) => {
                // TODO(#2589): Figure out how to know the status of the execution.
                //  We definitely can do it via providing all receipts and verifying
                //  the script root. But maybe we have less expensive way.
                let success_status = false;
                if success_status {
                    self.storage.storage_as_mut::<Messages>().remove(nonce)?;
                }
            }
        }

        Ok(())
    }

    fn process_output(
        &mut self,
        tx_pointer: TxPointer,
        utxo_id: UtxoId,
        inputs: &[Input],
        output: &Output,
    ) -> anyhow::Result<()> {
        match output {
            Output::Coin {
                to,
                amount,
                asset_id,
            }
            | Output::Change {
                to,
                amount,
                asset_id,
            }
            | Output::Variable {
                to,
                amount,
                asset_id,
            } => {
                self.insert_coin_if_it_has_amount(
                    tx_pointer, utxo_id, *to, *amount, *asset_id,
                )?;
            }
            Output::Contract(contract) => {
                self.try_insert_latest_contract_utxo(
                    tx_pointer, utxo_id, inputs, *contract,
                )?;
            }
            Output::ContractCreated { contract_id, .. } => {
                self.storage.storage::<ContractsLatestUtxo>().insert(
                    contract_id,
                    &ContractUtxoInfo::V1((utxo_id, tx_pointer).into()),
                )?;
            }
        }
        Ok(())
    }

    fn insert_coin_if_it_has_amount(
        &mut self,
        tx_pointer: TxPointer,
        utxo_id: UtxoId,
        owner: Address,
        amount: Word,
        asset_id: AssetId,
    ) -> anyhow::Result<()> {
        // Only insert a coin output if it has some amount.
        // This is because variable or transfer outputs won't have any value
        // if there's a revert or panic and shouldn't be added to the utxo set.
        if amount > Word::MIN {
            let coin = CompressedCoinV1 {
                owner,
                amount,
                asset_id,
                tx_pointer,
            }
            .into();

            self.storage.storage::<Coins>().insert(&utxo_id, &coin)?;
        }

        Ok(())
    }

    fn try_insert_latest_contract_utxo(
        &mut self,
        tx_pointer: TxPointer,
        utxo_id: UtxoId,
        inputs: &[Input],
        contract: output::contract::Contract,
    ) -> anyhow::Result<()> {
        if let Some(Input::Contract(input::contract::Contract { contract_id, .. })) =
            inputs.get(contract.input_index as usize)
        {
            self.storage.storage::<ContractsLatestUtxo>().insert(
                contract_id,
                &ContractUtxoInfo::V1((utxo_id, tx_pointer).into()),
            )?;
        } else {
            Err(ExecutorError::TransactionValidity(
                TransactionValidityError::InvalidContractInputIndex(utxo_id),
            ))?;
        }
        Ok(())
    }

    fn process_upgrade_transaction(
        &mut self,
        tx: &ChargeableTransaction<UpgradeBody, UpgradeMetadata>,
    ) -> anyhow::Result<()> {
        // This checks that the consensus parameters are valid.
        // Do we need this check, or can we assume that because the
        // transaction has been included into a block then the
        // metadata is valid?
        let Ok(metadata) = UpgradeMetadata::compute(tx) else {
            return Err(anyhow::anyhow!("Invalid upgrade metadata"));
        };

        match metadata {
            UpgradeMetadata::ConsensusParameters {
                consensus_parameters,
                calculated_checksum: _,
            } => {
                let Some(next_consensus_parameters_version) =
                    self.latest_consensus_parameters_version.checked_add(1)
                else {
                    return Err(anyhow::anyhow!("Invalid consensus parameters version"));
                };
                self.latest_consensus_parameters_version =
                    next_consensus_parameters_version;
                self.storage
                    .storage::<ConsensusParametersVersions>()
                    .insert(
                        &self.latest_consensus_parameters_version,
                        &consensus_parameters,
                    )?;
            }
            UpgradeMetadata::StateTransition => match tx.upgrade_purpose() {
                UpgradePurpose::ConsensusParameters { .. } => panic!(
                    "Upgrade with StateTransition metadata has StateTransition purpose"
                ),
                UpgradePurpose::StateTransition { root } => {
                    let Some(next_state_transition_bytecode_version) =
                        self.latest_state_transition_bytecode_version.checked_add(1)
                    else {
                        return Err(anyhow::anyhow!(
                            "Invalid state transition bytecode version"
                        ));
                    };
                    self.latest_state_transition_bytecode_version =
                        next_state_transition_bytecode_version;
                    self.storage
                        .storage::<StateTransitionBytecodeVersions>()
                        .insert(&self.latest_state_transition_bytecode_version, &root)?;
                }
            },
        }

        Ok(())
    }
}

pub trait TransactionInputs {
    fn inputs(&self) -> Cow<Vec<Input>>;
}

pub trait TransactionOutputs {
    fn outputs(&self) -> Cow<Vec<Output>>;
}

impl TransactionInputs for Transaction {
    fn inputs(&self) -> Cow<Vec<Input>> {
        match self {
            Transaction::Script(tx) => Cow::Borrowed(tx.inputs()),
            Transaction::Create(tx) => Cow::Borrowed(tx.inputs()),
            Transaction::Mint(tx) => {
                Cow::Owned(vec![Input::Contract(tx.input_contract().clone())])
            }
            Transaction::Upgrade(tx) => Cow::Borrowed(tx.inputs()),
            Transaction::Upload(tx) => Cow::Borrowed(tx.inputs()),
            Transaction::Blob(tx) => Cow::Borrowed(tx.inputs()),
        }
    }
}

impl TransactionOutputs for Transaction {
    fn outputs(&self) -> Cow<Vec<Output>> {
        match self {
            Transaction::Script(tx) => Cow::Borrowed(tx.outputs()),
            Transaction::Create(tx) => Cow::Borrowed(tx.outputs()),
            Transaction::Mint(tx) => {
                Cow::Owned(vec![Output::Contract(*tx.output_contract())])
            }
            Transaction::Upgrade(tx) => Cow::Borrowed(tx.outputs()),
            Transaction::Upload(tx) => Cow::Borrowed(tx.outputs()),
            Transaction::Blob(tx) => Cow::Borrowed(tx.outputs()),
        }
    }
}

// TODO(#2582): Add tests (https://github.com/FuelLabs/fuel-core/issues/2582)
#[test]
fn dummy() {}

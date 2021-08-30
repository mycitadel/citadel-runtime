// Citadel: Bitcoin, LN & RGB wallet runtime
// Written in 2021 by
//     Dr. Maxim Orlovsky <orlovsky@mycitadel.io>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the AGPL License
// along with this software.
// If not, see <https://www.gnu.org/licenses/agpl-3.0-standalone.html>.

use invoice::Invoice;

use bitcoin::secp256k1::rand::RngCore;
use bitcoin::{OutPoint, PublicKey, Transaction, TxIn, TxOut};
use chrono::{NaiveDateTime, Utc};
use electrum_client::{Client as ElectrumClient, ElectrumApi};
use lnpbp::seals::OutpointReveal;
use microservices::rpc::Failure;
use miniscript::{Descriptor, DescriptorTrait};
use rgb::{SealDefinition, SealEndpoint};
use rgb_node::rpc::reply::Transfer;
use std::collections::BTreeSet;
use wallet::psbt::{self, ProprietaryKey, ProprietaryWalletInput};
use wallet::{Psbt, PubkeyScript, Slice32};

use crate::cache::Driver as CacheDriver;
use crate::model::{
    ContractId, Operation, PaymentDirecton, Policy, PsbtWrapper,
    SpendingPolicy, TweakedOutput, Utxo,
};
use crate::rpc::message::{PreparedTransfer, RgbReceiver, TransferInfo};
use crate::runtime::Runtime;
use crate::storage::Driver as StorageDriver;
use crate::Error;

impl Runtime {
    pub(in crate::runtime) fn transfer(
        &mut self,
        pay_from: ContractId,
        asset_value: u64,
        bitcoin_fee: u64,
        transfer_info: TransferInfo,
        invoice: Invoice,
    ) -> Result<PreparedTransfer, Error> {
        let contract = self.storage.contract_ref(pay_from)?;
        let policy: Policy = contract.policy().clone();

        // For pure bitcoin transfers we must avoid using outputs
        // containing RGB assets
        // TODO: Support using RGB-containing outputs moving RGB assets
        //       if possible
        let mut coins = if transfer_info.is_rgb() {
            self.cache
                .unspent(pay_from)?
                .get(&transfer_info.contract_id())
                .cloned()
                .unwrap_or_default()
        } else {
            self.cache.unspent_bitcoin_only(pay_from)?
        }
        .into_iter()
        .collect::<Vec<_>>();

        // TODO: Implement more coin-selection strategies
        coins.sort_by(|a, b| a.value.cmp(&b.value));
        coins.reverse();

        trace!("Found coins: {:#?}", coins);

        // Collecting RGB witness/bitcoin payment inputs
        let mut asset_input_amount = 0u64;
        let asset_fee = if transfer_info.is_rgb() {
            0
        } else {
            bitcoin_fee
        };
        let balance_before = coins.iter().map(|utxo| utxo.value).sum();

        let mut asset_change_outpoint = None;
        let selected_utxos: Vec<Utxo> = coins
            .into_iter()
            .filter_map(|utxo| {
                if asset_input_amount >= asset_value + asset_fee {
                    debug!(
                        "Change value {} will be allocated to {}",
                        asset_input_amount - asset_value - asset_fee,
                        utxo.outpoint()
                    );
                    asset_change_outpoint =
                        asset_change_outpoint.or(Some(utxo.outpoint()));
                    return None;
                }
                if utxo.value == 0 {
                    return None;
                }
                asset_input_amount += utxo.value;
                trace!(
                    "Adding {} to the inputs with {} sats; total input value is {}",
                    utxo.outpoint(), utxo.value, asset_input_amount
                );
                Some(utxo)
            })
            .collect();
        let tx_inputs: Vec<TxIn> = selected_utxos
            .iter()
            .map(|utxo| TxIn {
                previous_output: utxo.outpoint(),
                script_sig: Default::default(),
                sequence: 0,
                witness: vec![],
            })
            .collect();
        if !transfer_info.is_rgb()
            && asset_input_amount < asset_value + bitcoin_fee
        {
            // TODO: Add more pure bitcoin inputs if there are not enough funds
            //       to pay bitcoin transaction fee
        }
        if asset_input_amount < asset_value + asset_fee {
            Err(Error::ServerFailure(Failure {
                code: 0,
                info: format!(
                    "Insufficient funds{}",
                    if transfer_info.is_rgb() {
                        ""
                    } else {
                        " on bitcoin outputs which do not have RGB assets on them"
                    }
                ),
            }))?;
        }

        // Constructing RGB witness/bitcoin payment transaction outputs
        let mut tx_outputs = vec![];
        let mut bitcoin_value = 0u64;
        let mut bitcoin_giveaway = None;
        let rgb_endpoint = if let Some(descriptor) =
            transfer_info.bitcoin_descriptor()
        {
            // We need this output only for bitcoin payments
            trace!("Adding output paying {} to {}", asset_value, descriptor);
            bitcoin_value = asset_value;
            tx_outputs.push((
                TxOut {
                    value: asset_value,
                    script_pubkey: PubkeyScript::from(descriptor).into(),
                },
                None,
            ));
            SealEndpoint::TxOutpoint(default!())
        } else if let TransferInfo::Rgb {
            contract_id,
            receiver:
                RgbReceiver::Descriptor {
                    ref descriptor,
                    giveaway,
                },
        } = transfer_info
        {
            // We need this output only for descriptor-based RGB payments
            trace!(
                "Adding output paying {} bitcoin giveaway to {}",
                giveaway,
                descriptor
            );
            bitcoin_giveaway = Some(giveaway);
            bitcoin_value = giveaway;
            tx_outputs.push((
                TxOut {
                    value: giveaway,
                    script_pubkey: PubkeyScript::from(descriptor.clone())
                        .into(),
                },
                None,
            ));
            SealEndpoint::with_vout(tx_outputs.len() as u32 - 1, &mut self.rng)
        } else if let TransferInfo::Rgb {
            contract_id: _,
            receiver: RgbReceiver::BlindUtxo(hash),
        } = transfer_info
        {
            SealEndpoint::TxOutpoint(hash)
        } else {
            unimplemented!()
        };
        debug!("RGB endpoint will be {:?}", rgb_endpoint);

        // Get to known how much bitcoins we are spending
        let all_unspent = self.cache.unspent(pay_from)?;
        let bitcoin_utxos = all_unspent
            .get(&rgb::ContractId::default())
            .ok_or(Error::CacheInconsistency)?;
        let outpoints = selected_utxos
            .iter()
            .map(Utxo::outpoint)
            .collect::<BTreeSet<_>>();
        let bitcoin_input_amount = bitcoin_utxos
            .iter()
            .filter(|bitcoin_utxo| outpoints.contains(&bitcoin_utxo.outpoint()))
            .fold(0u64, |sum, utxo| sum + utxo.value);

        // Adding bitcoin change output, if needed
        let mut output_derivation_indexes = set![];
        let (bitcoin_change, change_vout) = if bitcoin_input_amount
            > bitcoin_value + bitcoin_fee
        {
            let change = bitcoin_input_amount - bitcoin_value - bitcoin_fee;
            let change_index = self.cache.next_unused_derivation(pay_from)?;
            let change_address = contract
                .derive_address(change_index, false)
                .ok_or(Error::ServerFailure(Failure {
                    code: 0,
                    info: s!("Unable to derive change address"),
                }))?
                .address;
            self.cache.use_address_derivation(
                pay_from,
                change_address.clone(),
                change_index,
            )?;
            trace!(
                "Adding change output paying {} to our address {} at derivation index {}",
                change, change_address, change_index
            );
            tx_outputs.push((
                TxOut {
                    value: change,
                    script_pubkey: change_address.script_pubkey(),
                },
                Some(change_index),
            ));
            output_derivation_indexes.insert(change_index);
            (change, Some(tx_outputs.len() as u32 - 1))
        } else {
            (0, None)
        };

        // Adding RGB change output, if needed
        // NB: Right now, we use really dumb algorithm, allocating
        //     change to first found outpoint with existing assignment
        //     of the same asset, or, if none, to the bitcoin change
        //     output - failing if neither of them is present. We can
        //     be much smarter, assigning to existing bitcoin utxos,
        //     or creating new output for RGB change
        let mut rgb_change = bmap! {};
        if asset_input_amount > asset_value && transfer_info.is_rgb() {
            let change = asset_input_amount - asset_value;
            rgb_change.insert(
                asset_change_outpoint
                    .map(|outpoint| {
                        SealDefinition::TxOutpoint(OutpointReveal {
                            blinding: self.rng.next_u64(),
                            txid: outpoint.txid,
                            vout: outpoint.vout,
                        })
                    })
                    .or_else(|| {
                        change_vout.map(|vout| SealDefinition::WitnessVout {
                            vout,
                            blinding: self.rng.next_u64(),
                        })
                    })
                    .ok_or(Error::ServerFailure(Failure {
                        code: 0,
                        info: s!("Can't allocate RGB change"),
                    }))?,
                change,
            );
        }
        trace!("RGB change: {:?}", rgb_change);

        debug!(
            "Connecting electrum server at {} ...",
            self.config.electrum_server
        );
        debug!("Electrum server successfully connected");
        let electrum =
            ElectrumClient::new(&self.config.electrum_server.to_string())?;

        // Constructing bitcoin payment PSBT (for bitcoin payments) or
        // RGB witness PSBT prototype for the commitment (for RGB
        // payments)
        let psbt_inputs = tx_inputs
            .iter()
            .zip(&selected_utxos)
            .map(|(txin, utxo)| {
                let mut input = psbt::Input::default();
                // TODO: cache transactions
                input.non_witness_utxo =
                    electrum.transaction_get(&txin.previous_output.txid).ok();
                input.bip32_derivation =
                    policy.bip32_derivations(utxo.derivation_index);
                let script = policy
                    .derive_descriptor(utxo.derivation_index, false)
                    .as_ref()
                    .map(Descriptor::explicit_script);
                if policy.is_scripted() {
                    if policy.has_witness() {
                        input.witness_script = script;
                    } else {
                        input.redeem_script = script;
                    }
                }
                if let Some((tweak, pubkey)) = utxo.tweak {
                    input.p2c_tweak_add(pubkey, tweak);
                }
                input
            })
            .collect();
        let psbt_outputs = tx_outputs
            .iter()
            .map(|(txout, index)| {
                let mut output = psbt::Output::default();
                if let Some(index) = index {
                    output.proprietary.insert(
                        ProprietaryKey {
                            prefix: rgb::PSBT_PREFIX.to_vec(),
                            subtype: rgb::PSBT_OUT_PUBKEY,
                            key: vec![],
                        },
                        policy.first_public_key(*index).to_bytes(),
                    );
                }
                output
            })
            .collect();
        let psbt = Psbt {
            global: psbt::Global {
                unsigned_tx: Transaction {
                    version: 1,
                    lock_time: 0,
                    input: tx_inputs,
                    output: tx_outputs
                        .iter()
                        .map(|(txout, _)| txout.clone())
                        .collect(),
                },
                version: 0,
                xpub: none!(),
                proprietary: none!(),
                unknown: none!(),
            },
            inputs: psbt_inputs,
            outputs: psbt_outputs,
        };
        trace!("Prepared PSBT: {:#?}", psbt);

        // Committing to RGB transfer into the witness transaction and
        // producing consignments (applies to RGB payments only)
        let timestamp =
            NaiveDateTime::from_timestamp(Utc::now().timestamp(), 0);
        let payment_data = if let TransferInfo::Rgb {
            contract_id: asset_id,
            ref receiver,
        } = transfer_info
        {
            let Transfer {
                consignment,
                disclosure,
                witness,
            } = self.rgb20_client.transfer(
                asset_id,
                selected_utxos.iter().map(Utxo::outpoint).collect(),
                bmap! { rgb_endpoint => asset_value },
                rgb_change.clone(),
                psbt,
            )?;
            let txid = witness.global.unsigned_tx.txid();
            for (vout, out) in witness.outputs.iter().enumerate() {
                let tweak = out
                    .proprietary
                    .get(&ProprietaryKey {
                        prefix: rgb::PSBT_PREFIX.to_vec(),
                        subtype: rgb::PSBT_OUT_TWEAK,
                        key: vec![],
                    })
                    .and_then(Slice32::from_slice);
                let pubkey = out
                    .proprietary
                    .get(&ProprietaryKey {
                        prefix: rgb::PSBT_PREFIX.to_vec(),
                        subtype: rgb::PSBT_OUT_PUBKEY,
                        key: vec![],
                    })
                    .map(Vec::as_slice)
                    .map(PublicKey::from_slice)
                    .transpose()
                    .ok()
                    .flatten();
                let derivation_index = tx_outputs[vout].1;
                if let (Some(pubkey), Some(tweak), Some(derivation_index)) =
                    (pubkey, tweak, derivation_index)
                {
                    let tweaked_output = TweakedOutput {
                        outpoint: OutPoint::new(txid, vout as u32),
                        script: witness.global.unsigned_tx.output[vout]
                            .script_pubkey
                            .clone(),
                        tweak,
                        pubkey,
                        derivation_index,
                    };
                    debug!(
                        "Extracted tweak information from witness PSBT: {:?}",
                        tweaked_output
                    );
                    self.storage.add_p2c_tweak(pay_from, tweaked_output)?;
                }
            }

            // Self-enclosing disclosure.
            self.rgb20_client.enclose(disclosure.clone())?;

            // Creation history record
            let operation = Operation {
                txid: witness.global.unsigned_tx.txid(),
                direction: PaymentDirecton::Outcoming {
                    published: false,
                    asset_change: rgb_change.values().sum(),
                    bitcoin_change,
                    change_outputs: change_vout
                        .into_iter()
                        .map(|vout| vout as u16)
                        .collect(),
                    giveaway: bitcoin_giveaway,
                    paid_bitcoin_fee: bitcoin_fee,
                    output_derivation_indexes,
                    invoice,
                },
                created_at: timestamp,
                height: 0,
                asset_id: None,
                balance_before,
                bitcoin_volume: bitcoin_input_amount,
                asset_volume: asset_input_amount,
                bitcoin_value,
                asset_value,
                tx_fee: bitcoin_fee,
                psbt: PsbtWrapper(witness.clone()),
                disclosure: Some(disclosure),
                notes: None,
            };
            trace!(
                "Creating operation for the history record: {:#?}",
                operation
            );
            self.storage.register_operation(pay_from, operation)?;

            trace!("Witness PSBT: {:#?}", witness);
            PreparedTransfer {
                psbt: witness,
                consignment: Some(consignment),
            }
        } else {
            // Creation history record
            let operation = Operation {
                txid: psbt.global.unsigned_tx.txid(),
                direction: PaymentDirecton::Outcoming {
                    published: false,
                    asset_change: bitcoin_change,
                    bitcoin_change,
                    change_outputs: change_vout
                        .into_iter()
                        .map(|vout| vout as u16)
                        .collect(),
                    giveaway: None,
                    paid_bitcoin_fee: bitcoin_fee,
                    output_derivation_indexes,
                    invoice,
                },
                created_at: timestamp,
                height: 0,
                asset_id: None,
                balance_before,
                bitcoin_volume: bitcoin_input_amount,
                asset_volume: bitcoin_input_amount,
                bitcoin_value,
                asset_value,
                tx_fee: bitcoin_fee,
                psbt: PsbtWrapper(psbt.clone()),
                disclosure: None,
                notes: None,
            };
            trace!(
                "Creating operation for the history record: {:#?}",
                operation
            );
            self.storage.register_operation(pay_from, operation)?;

            // TODO: If any of bitcoin inputs contain some RGB assets
            //       we must do an "internal transfer"
            PreparedTransfer {
                psbt,
                consignment: None,
            }
        };

        Ok(payment_data)
    }
}

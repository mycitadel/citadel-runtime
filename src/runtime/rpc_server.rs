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

use bp::seals::OutpointReveal;
use electrum_client::{Client as ElectrumClient, ElectrumApi};
use internet2::{TypedEnum, Unmarshall};
use microservices::rpc::Failure;
use microservices::FileFormat;
use rgb::{SealEndpoint, Validity};
use rgb20::Asset;
use rgb_node::rpc::reply::SyncFormat;
use rgb_node::util::ToBech32Data;
use strict_encoding::StrictDecode;
use wallet::descriptors::ContractDescriptor;

use super::Runtime;
use crate::cache::Driver as CacheDriver;
use crate::model::{Contract, ContractMeta, Policy, SpendingPolicy};
use crate::rpc::{message, Reply, Request};
use crate::storage::Driver as StorageDriver;
use crate::Error;
use crate::SECP256K1;

impl Runtime {
    pub(super) fn rpc_process(&mut self, raw: Vec<u8>) -> Result<Reply, Reply> {
        trace!(
            "Got {} bytes over ZMQ RPC: {}",
            raw.len(),
            raw.to_bech32data()
        );
        let message = (&*self.unmarshaller.unmarshall(&raw)?).clone();
        debug!(
            "Received ZMQ RPC request #{}: {}",
            message.get_type(),
            message
        );
        match message {
            Request::CreateSingleSig(req) => {
                let contract = Contract::with(
                    Policy::Current(ContractDescriptor::SingleSig {
                        category: req.category,
                        pk: req.pubkey_chain,
                    }),
                    req.name,
                    self.config.chain.clone(),
                );
                self.storage
                    .add_contract(contract)
                    .map(ContractMeta::from)
                    .map(Reply::Contract)
                    .map_err(Error::from)
            }

            Request::ContractOperations(contract_id) => self
                .storage
                .contract_ref(contract_id)
                .map(|contract| contract.history())
                .map(Reply::Operations)
                .map_err(Error::from),

            Request::ListContracts => self
                .storage
                .contracts()
                .map(|vec| vec.into_iter().map(ContractMeta::from).collect::<Vec<_>>())
                .map(Reply::Contracts)
                .map_err(Error::from),

            Request::RenameContract(message::RenameContractRequest {
                contract_id,
                name,
            }) => self
                .storage
                .rename_contract(contract_id, name)
                .map(|_| Reply::Success)
                .map_err(Error::from),

            Request::DeleteContract(contract_id) => self
                .storage
                .delete_contract(contract_id)
                .map(|_| Reply::Success)
                .map_err(Error::from),

            Request::SyncContract(message::SyncContractRequest {
                contract_id,
                lookup_depth,
            }) => {
                let assets = self.chain_sync(contract_id, lookup_depth)?;
                Ok(Reply::ContractUnspent(assets))
            }

            Request::UsedAddresses(contract_id) => self
                .cache
                .used_address_derivations(contract_id)
                .map(Reply::Addresses)
                .map_err(Error::from),

            Request::NextAddress(message::NextAddressRequest {
                contract_id,
                index,
                legacy,
                mark_used,
            }) => self
                .storage
                .contract_ref(contract_id)
                .map_err(Error::from)?
                .derive_address(
                    index.unwrap_or(
                        self.cache
                            .next_unused_derivation(contract_id)
                            .map_err(Error::from)?,
                    ),
                    legacy,
                )
                .and_then(|address_derivation| {
                    if mark_used {
                        self.cache.use_address_derivation(
                            contract_id,
                            address_derivation.address.clone(),
                            *address_derivation.derivation.last().expect(
                                "derivation path must always have at least one element"
                            ),
                        ).ok()?;
                    }
                    Some(address_derivation)
                })
                .map(Reply::AddressDerivation)
                .ok_or(Error::ServerFailure(Failure {
                    code: 0,
                    info: s!("Unable to derive address for the provided network/chain"),
                })),

            Request::UnuseAddress(message::ContractAddressTuple {
                contract_id,
                address,
            }) => self
                .cache
                .forget_address(contract_id, &address)
                .map(|_| Reply::Success)
                .map_err(Error::from),

            Request::BlindUtxo(contract_id) => self
                .cache
                .utxo(contract_id)
                .map_err(Error::from)
                .and_then(|utxo| {
                    utxo.into_iter().next().ok_or(Error::ServerFailure(
                        Failure {
                            code: 0,
                            info: s!("No UTXO available"),
                        },
                    ))
                })
                .map(|outpoint| OutpointReveal::from(outpoint))
                .map(Reply::BlindUtxo),

            Request::ListInvoices(contract_id) => {
                self.storage
                    .contract_ref(contract_id)
                    .map(|contract| contract.data().sent_invoices().clone())
                    .map(Reply::Invoices)
                    .map_err(Error::from)
            },

            Request::AddInvoice(message::AddInvoiceRequest { invoice, source_info }) => {
                for (contract_id, outpoint_reveal) in source_info {
                    self.storage.add_invoice(
                        contract_id,
                        invoice.clone(),
                        outpoint_reveal.map(|r| vec![r]).unwrap_or_default()
                    ).map_err(Error::from)?;
                }
                Ok(Reply::Success)
            },

            Request::ComposeTransfer(message::ComposeTransferRequest { pay_from, asset_value, bitcoin_fee, transfer_info, invoice }) => {
                let payment_data = self.transfer(pay_from, asset_value, bitcoin_fee, transfer_info, invoice)?;
                Ok(Reply::PreparedPayment(payment_data))
            },

            Request::FinalizeTransfer(mut psbt) => {
                debug!("Finalizing the provided PSBT");
                match miniscript::psbt::finalize(&mut psbt, &*SECP256K1)
                    .and_then(|_| miniscript::psbt::extract(&psbt, &*SECP256K1)) {
                    Ok(tx) => {
                        // TODO: Update saved PSBT
                        trace!("Finalized PSBT: {:#?}", psbt);

                        debug!(
                            "Connecting electrum server at {} ...",
                            self.config.electrum_server
                        );
                        debug!("Electrum server successfully connected");
                        let electrum =
                            ElectrumClient::new(&self.config.electrum_server.to_string()).map_err(Error::from)?;

                        debug!("Publishing transaction to bitcoin network via Electrum server");
                        trace!("{:#?}", tx);
                        electrum
                            .transaction_broadcast(&tx)
                            .map(|_| Reply::Success)
                            .map_err(|err| {
                                error!("Electrum server error: {:?}", err);
                                err
                            })
                            .map_err(Error::from)
                    }
                    Err(err) => {
                        error!("Error finalizing PSBT: {}", err);
                        Ok(Reply::Failure(Failure {
                            code: 0,
                            info: err.to_string()
                        }))
                    }
                }
            }

            Request::AcceptTransfer(consignment) => {
                let status = self.rgb20_client.validate(consignment.clone()).map_err(Error::from)?;
                if status.validity() == Validity::Valid {
                    let hashes = consignment.endpoints.iter().filter_map(|(_, seal_endpoint)| match seal_endpoint {
                        SealEndpoint::TxOutpoint(hash) => Some(*hash),
                        SealEndpoint::WitnessVout { .. } => None,
                    }).collect::<Vec<_>>();
                    let revel_outpoints = self.storage
                        .contracts().map_err(Error::from)?
                        .iter()
                        .flat_map(|contract| contract.data().blinding_factors())
                        .filter_map(|(hash, reveal)| {
                            if hashes.contains(hash) {
                                Some(*reveal)
                            } else {
                                None
                            }
                        }).collect();
                    self.rgb20_client.accept(consignment, revel_outpoints).map_err(Error::from)?;
                }
                Ok(Reply::Validation(status))
            }

            Request::ContractUnspent(id) => self
                .cache
                .unspent(id)
                .map(|arg| arg.into_iter().map(|(k, v)| (k, v.into_iter().collect())).collect())
                .map(Reply::ContractUnspent)
                .map_err(Error::from),

            Request::ListIdentities => self
                .storage
                .identities()
                .map(Reply::Identities)
                .map_err(Error::from),

            Request::AddSigner(account) => self
                .storage
                .add_signer(account)
                .map(|_| Reply::Success)
                .map_err(Error::from),

            Request::AddIdentity(identity) => self
                .storage
                .add_identity(identity)
                .map(|_| Reply::Success)
                .map_err(Error::from),

            Request::ImportAsset(genesis) => self
                .rgb20_client
                .import_asset(genesis)
                .map(Reply::Asset)
                .map_err(Error::from),

            Request::ListAssets => self
                .rgb20_client
                .list_assets(FileFormat::StrictEncode)
                .map_err(Error::from)
                .and_then(|SyncFormat(_, data)| {
                    Vec::<Asset>::strict_deserialize(data).map_err(Error::from)
                })
                .map(Reply::Assets),

        }
        .map_err(Error::into)
    }
}

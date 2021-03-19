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

use std::collections::{BTreeMap, BTreeSet};
use std::convert::TryInto;

use bitcoin::{OutPoint, Script, Txid};
use electrum_client::{Client as ElectrumClient, ElectrumApi};
use wallet::bip32::{ChildIndex, UnhardenedIndex};
use wallet::AddressCompat;

use crate::cache::Driver as CacheDriver;
use crate::model::{ContractId, TweakedOutput, Utxo};
use crate::runtime::Runtime;
use crate::storage::Driver as StorageDriver;
use crate::Error;

impl Runtime {
    pub(in crate::runtime) fn chain_sync(
        &mut self,
        contract_id: ContractId,
        lookup_depth: u8,
    ) -> Result<BTreeMap<rgb::ContractId, Vec<Utxo>>, Error> {
        debug!("Synchronizing contract data with electrum server");

        debug!(
            "Connecting electrum server at {} ...",
            self.config.electrum_server
        );
        debug!("Electrum server successfully connected");
        let electrum =
            ElectrumClient::new(&self.config.electrum_server.to_string())?;

        let lookup_depth = UnhardenedIndex::from(lookup_depth);

        let contract = self.storage.contract_ref(contract_id)?;
        let policy = self.storage.policy(contract_id)?;

        let mut unspent: Vec<Utxo> = vec![];
        let mut outpoints: BTreeSet<OutPoint> = bset![];
        let mut mine_info: BTreeMap<(u32, u16), Txid> = bmap! {};

        let mut index_offset = UnhardenedIndex::zero();
        let last_used_index = self
            .cache
            .last_used_derivation(contract_id)
            .unwrap_or_default();

        let mut scripts: Vec<(UnhardenedIndex, Script, Option<TweakedOutput>)> =
            contract
                .data()
                .p2c_tweaks()
                .into_iter()
                .map(|tweak| {
                    (
                        tweak.derivation_index,
                        tweak.script.clone(),
                        Some(tweak.clone()),
                    )
                })
                .collect();
        debug!(
            "Requesting unspent information for {} known tweaked scripts",
            scripts.len()
        );

        loop {
            let mut count = 0usize;
            trace!("{:#?}", scripts);

            let txid_map = electrum
                .batch_script_list_unspent(
                    &scripts
                        .iter()
                        .map(|(_, script, _)| script.clone())
                        .collect::<Vec<_>>(),
                )
                .map_err(|_| Error::Electrum)?
                .into_iter()
                .zip(scripts)
                .fold(
                    BTreeMap::<
                        (u32, Txid),
                        Vec<(
                            u16,
                            u64,
                            UnhardenedIndex,
                            Script,
                            Option<TweakedOutput>,
                        )>,
                    >::new(),
                    |mut map, (found, (derivation_index, script, tweak))| {
                        for item in found {
                            map.entry((item.height as u32, item.tx_hash))
                                .or_insert(Vec::new())
                                .push((
                                    item.tx_pos as u16,
                                    item.value,
                                    derivation_index,
                                    script.clone(),
                                    tweak.clone(),
                                ));
                            count += 1;
                        }
                        map
                    },
                );
            debug!("Found {} unspent outputs in the batch", count);
            trace!("{:#?}", txid_map);

            trace!(
                "Resolving block transaction position for {} transactions",
                txid_map.len()
            );
            for ((height, txid), outs) in txid_map {
                match electrum.transaction_get_merkle(&txid, height as usize) {
                    Ok(res) => {
                        mine_info.insert((height, res.pos as u16), txid);
                        for (vout, value, derivation_index, script, tweak) in
                            outs
                        {
                            if !outpoints
                                .insert(OutPoint::new(txid, vout as u32))
                            {
                                continue;
                            }
                            let address = contract
                                .chain()
                                .try_into()
                                .ok()
                                .and_then(|network| {
                                    AddressCompat::from_script(&script, network)
                                });
                            unspent.push(Utxo {
                                value,
                                height,
                                offset: res.pos as u16,
                                txid,
                                vout,
                                derivation_index,
                                tweak: tweak
                                    .map(|tweak| (tweak.tweak, tweak.pubkey)),
                                address,
                            });
                        }
                    }
                    Err(err) => warn!(
                        "Unable to get tx block position for {} at height {}: \
                        electrum server error {:?}",
                        txid, height, err
                    ),
                }
            }

            if count == 0 && index_offset > last_used_index {
                debug!(
                    "No unspent outputs are found in the batch and we \
                            are behind the last used derivation; stopping search"
                );
                break;
            }

            if index_offset == UnhardenedIndex::largest() {
                debug!("Reached last possible index number, breaking");
                break;
            }
            let from = index_offset;
            index_offset = index_offset
                .checked_add(lookup_depth)
                .unwrap_or(UnhardenedIndex::largest());
            scripts = policy
                .derive_scripts(from..index_offset)
                .into_iter()
                .map(|(derivation_index, script)| {
                    (derivation_index, script, None)
                })
                .collect();
            debug!("Generating next spending script batch");
        }

        while let Ok(Some(info)) = electrum.block_headers_pop() {
            debug!("Updating known blockchain height: {}", info.height);
            self.known_height = info.height as u32;
        }

        let mut assets =
            bmap! { rgb::ContractId::default() => unspent.clone() };
        for (utxo, outpoint) in unspent.iter_mut().zip(outpoints.iter()) {
            for (asset_id, amounts) in
                self.rgb20_client.outpoint_assets(*outpoint)?
            {
                if amounts.is_empty() {
                    continue;
                }
                let amount = amounts.iter().sum();
                if amount > 0 {
                    let mut u = utxo.clone();
                    u.value = amount;
                    assets.entry(asset_id).or_insert(vec![]).push(u);
                }
            }
        }

        trace!("Transaction mining info: {:#?}", mine_info);
        self.cache.update(
            contract_id,
            mine_info,
            Some(self.known_height),
            outpoints,
            assets.clone(),
        )?;

        Ok(assets)
    }
}

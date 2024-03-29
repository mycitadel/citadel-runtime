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

use serde_with::DisplayFromStr;
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::io;
use std::ops::Range;

use bitcoin::util::bip32::KeySource;
use bitcoin::Script;
use commit_verify::{CommitEncode, ConsensusCommit};
use internet2::RemoteNodeAddr;
use lnp::ChannelId;
use lnpbp::chain::Chain;
use miniscript::descriptor::DescriptorType;
use miniscript::{
    descriptor, Descriptor, DescriptorTrait, ForEach, ForEachKey, TranslatePk2,
};
use strict_encoding::{self, StrictDecode, StrictEncode};
use wallet::descriptors::ContractDescriptor;
use wallet::hd::{ChildIndex, PubkeyChain, TerminalStep, UnhardenedIndex};

use super::ContractId;
use crate::model::AddressDerivation;
use crate::SECP256K1;

/// Defines a type of a wallet contract basing on the banking use case,
/// abstracting the underlying technology(ies) into specific contract details
#[derive(
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    Display,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PolicyType {
    /// Accounts that allow spending with a simple procedure (like single
    /// signature). However the actual transfer may take some time (like mining
    /// onchain transaction). Analogous to "paying with gold coins" or
    /// "doing a SWFIT/SEPA transfer". May require use of hardware wallet
    /// devices
    #[display("current")]
    Current,

    /// Instant payment accounts allowing simple & fasm payments with strict
    /// limits. Must not require any hardware security device for processing.
    /// The main technology is the Lightning network, with different forms
    /// of fast payment channels on top of it (currently only BOLT-3-based).
    /// Analogous to credit cards payments and instant payment systems
    /// (PayPal, QIWI etc).
    #[display("instant")]
    Instant,

    /// Accounts with complex spending processes, requiring hardware devices,
    /// multiple signatures, timelocks and other forms of limitations.
    #[display("saving")]
    Saving,

    /// Future forms of smart-contracts for borrowing money and assets. Will
    /// probably require some advanced smart contract technology, like
    /// new forms of scriptless scripts and/or RGB schemata + simplicity
    /// scripting.
    #[display("loan")]
    Loan,

    /// May also be used for providing funds to liquidity pools etc.
    #[display("staking")]
    Staking,

    #[display("trading")]
    Trading,

    #[display("storage")]
    Storage,

    #[display("computing")]
    Computing,
}

#[serde_as]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    Display,
    StrictEncode,
    StrictDecode,
)]
#[non_exhaustive]
#[display(inner)]
pub enum Policy {
    Current(#[serde_as(as = "DisplayFromStr")] ContractDescriptor<PubkeyChain>),

    Instant(ChannelDescriptor),

    Saving(#[serde_as(as = "DisplayFromStr")] ContractDescriptor<PubkeyChain>),
}

impl ConsensusCommit for Policy {
    type Commitment = ContractId;
}

impl CommitEncode for Policy {
    fn commit_encode<E: io::Write>(&self, e: E) -> usize {
        self.strict_encode(e)
            .expect("Memory encoders does not fail")
    }
}

impl Policy {
    pub fn id(&self) -> ContractId {
        self.clone().consensus_commit()
    }

    pub fn policy_type(&self) -> PolicyType {
        match self {
            Policy::Current { .. } => PolicyType::Current,
            Policy::Instant { .. } => PolicyType::Instant,
            Policy::Saving { .. } => PolicyType::Saving,
        }
    }

    pub fn is_scripted(&self) -> bool {
        match self {
            Policy::Current(ContractDescriptor::SingleSig { .. }) => false,
            _ => true,
        }
    }

    pub fn has_witness(&self) -> bool {
        match self {
            Policy::Instant { .. } => true,
            _ => match self.to_descriptor().desc_type() {
                DescriptorType::Bare
                | DescriptorType::Sh
                | DescriptorType::Pkh
                | DescriptorType::ShSortedMulti => false,
                DescriptorType::Wpkh
                | DescriptorType::Wsh
                | DescriptorType::ShWsh
                | DescriptorType::ShWpkh
                | DescriptorType::WshSortedMulti
                | DescriptorType::ShWshSortedMulti => true,
            },
        }
    }

    pub fn to_descriptor(&self) -> Descriptor<PubkeyChain> {
        match self {
            Policy::Current(descriptor) => descriptor.to_descriptor(false),
            Policy::Instant(channel) => channel.to_descriptor(),
            Policy::Saving(descriptor) => descriptor.to_descriptor(false),
        }
    }

    fn translate(
        d: &Descriptor<PubkeyChain>,
        index: UnhardenedIndex,
    ) -> Descriptor<bitcoin::PublicKey> {
        d.translate_pk2_infallible(|chain| {
            // TODO: Add convenience PubkeyChain methods
            let mut path = chain.terminal_path.clone();
            if path.last() == Some(&TerminalStep::Wildcard) {
                path.remove(path.len() - 1);
            }
            path.push(TerminalStep::Index(index.into()));
            chain.derive_pubkey(&*SECP256K1, Some(index))
        })
    }

    pub fn pubkey_chains(&self) -> Vec<PubkeyChain> {
        let mut collected = vec![];
        self.to_descriptor().for_each_key(|key| {
            if let ForEach::Key(pubkey_chain) = key {
                collected.push(pubkey_chain.clone())
            }
            true
        });
        collected
    }

    pub fn bip32_derivations(
        &self,
        index: UnhardenedIndex,
    ) -> BTreeMap<bitcoin::PublicKey, KeySource> {
        self.pubkey_chains()
            .into_iter()
            .map(|pubkey_chain| {
                pubkey_chain.bip32_derivation(&*SECP256K1, Some(index))
            })
            .collect()
    }

    pub fn first_public_key(
        &self,
        index: UnhardenedIndex,
    ) -> bitcoin::PublicKey {
        self.pubkey_chains()
            .first()
            .expect("Descriptor must contain at least one signing key")
            .derive_pubkey(&*SECP256K1, Some(index))
    }

    pub fn derive_scripts(
        &self,
        range: Range<UnhardenedIndex>,
    ) -> BTreeMap<UnhardenedIndex, Script> {
        let mut script_map = bmap![];
        let d = self.to_descriptor();
        let mut index = range.start;
        while index < range.end {
            script_map
                .insert(index, Self::translate(&d, index).script_pubkey());
            index
                .checked_inc_assign()
                .expect("UnhardenedIndex ranges are broken");
        }
        script_map
    }

    pub fn derive_descriptor(
        &self,
        index: UnhardenedIndex,
        legacy: bool,
    ) -> Option<Descriptor<bitcoin::PublicKey>> {
        let mut d = self.to_descriptor();
        // TODO: Propose a PR to rust-miniscript with `to_nested()` method
        if legacy {
            d = match d {
                Descriptor::Wpkh(wpkh) => Descriptor::Sh(
                    descriptor::Sh::new_wpkh(wpkh.into_inner()).ok()?,
                ),
                Descriptor::Wsh(wsh) => match wsh.into_inner() {
                    descriptor::WshInner::Ms(ms) => {
                        Descriptor::Sh(descriptor::Sh::new_wsh(ms).ok()?)
                    }
                    descriptor::WshInner::SortedMulti(smv) => Descriptor::Sh(
                        descriptor::Sh::new_sortedmulti(smv.k, smv.pks).ok()?,
                    ),
                },
                _ => d,
            };
        }
        Some(Self::translate(&d, index.into()))
    }

    pub fn derive_address(
        &self,
        index: UnhardenedIndex,
        chain: &Chain,
        legacy: bool,
    ) -> Option<AddressDerivation> {
        self.derive_descriptor(index, legacy)
            .and_then(|d| chain.try_into().ok().map(|network| (d, network)))
            .and_then(|(d, network)| d.address(network).ok())
            .map(|address| AddressDerivation::with(address, vec![index]))
    }
}

#[serde_as]
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Hash,
    Debug,
    Display,
    StrictEncode,
    StrictDecode,
)]
#[display("{channel_id}")]
pub struct ChannelDescriptor {
    channel_id: ChannelId,

    #[serde_as(as = "Vec<DisplayFromStr>")]
    peers: Vec<RemoteNodeAddr>,
}

impl ChannelDescriptor {
    // TODO: Store base points in the channel descriptor and use them to derive
    //       descriptors for all channel transaction outputs to monitor their
    //       onchain status
    pub fn to_descriptor(&self) -> Descriptor<PubkeyChain> {
        unimplemented!()
    }
}

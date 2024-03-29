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

use std::str::FromStr;

use amplify::Wrapper;
use bitcoin::hashes::{sha256, sha256t};
use commit_verify::{tagged_hash, CommitVerify, TaggedHash};
use lnpbp::bech32::{FromBech32IdStr, ToBech32IdString};
use strict_encoding::{self, StrictDecode, StrictEncode};

pub struct ContractIdTag;

impl sha256t::Tag for ContractIdTag {
    #[inline]
    fn engine() -> sha256::HashEngine {
        let midstate = sha256::Midstate::from_inner(
            tagged_hash::Midstate::with("citadel:contract")
                .into_inner()
                .into_inner(),
        );
        sha256::HashEngine::from_midstate(midstate, 64)
    }
}

#[derive(
    Serialize,
    Deserialize,
    Wrapper,
    Copy,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Display,
    Default,
    From,
    StrictEncode,
    StrictDecode,
)]
#[wrapper(Debug, LowerHex, Index, IndexRange, IndexFrom, IndexTo, IndexFull)]
#[display(ContractId::to_bech32_id_string)]
pub struct ContractId(sha256t::Hash<ContractIdTag>);

impl<MSG> CommitVerify<MSG> for ContractId
where
    MSG: AsRef<[u8]>,
{
    #[inline]
    fn commit(msg: &MSG) -> ContractId {
        <ContractId as TaggedHash<_>>::hash(msg)
    }
}

impl FromStr for ContractId {
    type Err = lnpbp::bech32::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ContractId::from_bech32_id_str(s)
    }
}

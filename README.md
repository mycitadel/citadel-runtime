# Citadel: Bitcoin, LN & RGB wallet

Library providing runtime for Bitcoin, LN & RGB wallet

Supports:
- Arbitrary-complex descriptor-based wallets
- Miniscript-based wallet spending policies
- Complex multisignature schemes
- Lightning network (in generalized format, with planned support for
  multi-peer channels, DEX etc)
- [RGB assets](https://www.rgbfaq.com/faq/untitled)
- [Universal invoices](https://github.com/LNP-BP/FAQ/blob/master/Presentation%20slides/Universal%20LNP-BP%20invoices.pdf), 
  supporting on-chain descriptor- and PSBT-based invoices, Lightning network,
  RGB assets, repeated payments, multiple beneficiaries etc...
- Taproot & Schnorr signatures (WIP)
- Partially signed bitcoin transactions
- Arbitrary complex derivation paths, including new identity-based
  derivations for multisigs and taproot 
  (see https://lists.linuxfoundation.org/pipermail/bitcoin-dev/2021-February/018381.html)
- Complete separation of private keys to external HSMs or multiple signature
  servers etc (via PSBTs)

Citadel librsry demonstrates how modern censorship-resistant self-sovereign 
privacy-focusing P2P software can be developed basing on
[LNP/BP Association](https://github.com/LNP-BP) standards, frameworks and
libraries for LNP/BP & Internet2 protocols.

The runtime can run (in embedded mode) or connect to external 
[RGB Node](https://github.com/rgb-org/rgb-node) and 
[LNP Node](https://github.com/LNP-BP/lnp-node), plus it require
external Electrum Server accessed via ElectrumX protocol (in future will be
replaced by [BP Node](https://github.com/LNP-BP/bp-node)).

Citadel library is based on:
- [Descriptor wallet](https://github.com/LNP-BP/descriptor-wallet) native rust
  bitcoin wallet library
- [Internet2 suite of protocols](https://github.com/internet2-org/rust-internet2) 
- [Web4 microservice framework](https://crates.io/crates/microservices) from 
  the same repo

## Design & architecture

High-level architecture:
![Wallet architecture](doc/assets/architecture.png)

More details on modules:
![Wallet components](doc/assets/components.png)

Microservice architecture
![Microservices](doc/assets/microservices.png)

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

use bitcoin::secp256k1::rand::rngs::ThreadRng;
use electrum_client::{Client as ElectrumClient, ElectrumApi};
use internet2::{
    session, zmqsocket, CreateUnmarshaller, PlainTranscoder, Session,
    TypedEnum, Unmarshaller, ZmqSocketAddr, ZmqType,
};
use microservices::node::TryService;

use super::Config;
use crate::rpc::Request;
use crate::{cache, storage, Error};

pub fn run(config: Config) -> Result<(), Error> {
    let runtime = Runtime::init(config)?;

    runtime.run_or_panic("citadeld");

    Ok(())
}

pub struct Runtime {
    /// Original configuration object
    pub(super) config: Config,

    /// Stored sessions
    pub(super) session_rpc:
        session::Raw<PlainTranscoder, zmqsocket::Connection>,

    /// Wallet data storage
    pub(super) storage: storage::FileDriver,

    /// Wallet data cache
    pub(super) cache: cache::FileDriver,

    /// Unmarshaller instance used for parsing RPC request
    pub(super) unmarshaller: Unmarshaller<Request>,

    /// RGB20 (fungibled) daemon client
    pub(super) rgb20_client: rgb_node::i9n::Runtime,

    /// Random number generator (used in creation of blinding secrets)
    pub(super) rng: ThreadRng,

    /// Known blockchain height by the last received block header
    pub(super) known_height: u32,
}

impl Runtime {
    pub fn init(config: Config) -> Result<Self, Error> {
        debug!("Initializing wallet storage {:?}", config.storage_conf());
        let storage = storage::FileDriver::with(config.storage_conf())?;

        debug!("Initializing wallet cache {:?}", config.cache_conf());
        let cache = cache::FileDriver::with(config.cache_conf())?;

        debug!("Initializing random number generator");
        let rng = bitcoin::secp256k1::rand::thread_rng();

        debug!("Opening RPC API socket {}", config.rpc_endpoint);
        let session_rpc = session::Raw::with_zmq_unencrypted(
            ZmqType::Rep,
            &config.rpc_endpoint,
            None,
            None,
        )?;

        debug!(
            "Connecting electrum server at {} ...",
            config.electrum_server
        );
        debug!("Electrum server successfully connected");
        let electrum =
            ElectrumClient::new(&config.electrum_server.to_string())?;
        debug!("Subscribing to new block notifications");
        let known_height = electrum.block_headers_subscribe()?.height as u32;

        let rgb_config = rgb_node::i9n::Config {
            verbose: config.verbose,
            data_dir: config.data_dir.clone().to_string_lossy().to_string(),
            electrum_server: config.electrum_server.clone(),
            stash_rpc_endpoint: ZmqSocketAddr::Inproc(s!("stash.rpc")),
            contract_endpoints: map! {
                rgb_node::rgbd::ContractName::Fungible => config.rgb20_endpoint.clone()
            },
            network: config.chain.clone(),
            run_embedded: config.rgb_embedded,
        };
        debug!(
            "Connecting RGB node embedded runtime using config {}...",
            rgb_config
        );
        let rgb20_client = rgb_node::i9n::Runtime::init(rgb_config)
            .map_err(|_| Error::EmbeddedNodeInitError)?;
        debug!("RGB node runtime successfully connected");

        info!("Citadel runtime started successfully");

        Ok(Self {
            config,
            session_rpc,
            storage,
            cache,
            rgb20_client,
            rng,
            unmarshaller: Request::create_unmarshaller(),
            known_height,
        })
    }
}

impl TryService for Runtime {
    type ErrorType = Error;

    fn try_run_loop(mut self) -> Result<(), Self::ErrorType> {
        loop {
            match self.run() {
                Ok(_) => debug!("API request processing complete"),
                Err(err) => {
                    error!("Error processing API request: {}", err);
                    Err(err)?;
                }
            }
        }
    }
}

impl Runtime {
    fn run(&mut self) -> Result<(), Error> {
        trace!("Awaiting for ZMQ RPC requests...");
        let raw = self.session_rpc.recv_raw_message()?;
        let reply = self.rpc_process(raw).unwrap_or_else(|err| err);
        trace!("Preparing ZMQ RPC reply: {:?}", reply);
        let data = reply.serialize();
        trace!(
            "Sending {} bytes back to the client over ZMQ RPC",
            data.len()
        );
        self.session_rpc.send_raw_message(&data)?;
        Ok(())
    }
}

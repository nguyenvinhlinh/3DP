//! Service and ServiceFactory implementation. Specialized wrapper over substrate service.
#![allow(clippy::needless_borrow)]
use runtime::{self, opaque::Block, RuntimeApi};
use sc_client_api::{ExecutorProvider, RemoteBackend};
use sc_executor::native_executor_instance;
pub use sc_executor::NativeExecutor;
use sc_finality_grandpa::GrandpaBlockImport;
use sc_service::{error::Error as ServiceError, Configuration, PartialComponents, TaskManager};
use sha3pow::*;
use sp_api::TransactionFor;
use sp_consensus::import_queue::BasicQueue;
use sp_core::{Encode, U256, H256};
use sp_inherents::InherentDataProviders;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
// use sp_runtime::generic::BlockId;
use sc_consensus_poscan::PoscanData;

// Our native executor instance.
native_executor_instance!(
	pub Executor,
	runtime::api::dispatch,
	runtime::native_version,
);

type FullClient = sc_service::TFullClient<Block, RuntimeApi, Executor>;
type FullBackend = sc_service::TFullBackend<Block>;
type FullSelectChain = sc_consensus::LongestChain<FullBackend, Block>;

pub fn build_inherent_data_providers() -> Result<InherentDataProviders, ServiceError> {
	let providers = InherentDataProviders::new();

	providers
		.register_provider(sp_timestamp::InherentDataProvider)
		.map_err(Into::into)
		.map_err(sp_consensus::error::Error::InherentData)?;

	Ok(providers)
}

/// Returns most parts of a service. Not enough to run a full chain,
/// But enough to perform chain operations like purge-chain
#[allow(clippy::type_complexity)]
pub fn new_partial(
	config: &Configuration,
) -> Result<
	PartialComponents<
		FullClient,
		FullBackend,
		FullSelectChain,
		BasicQueue<Block, TransactionFor<FullClient, Block>>,
		sc_transaction_pool::FullPool<Block, FullClient>,
		(
			sc_consensus_poscan::PowBlockImport<
				Block,
				GrandpaBlockImport<FullBackend, Block, FullClient, FullSelectChain>,
				FullClient,
				FullSelectChain,
				PoscanAlgorithm,
				impl sp_consensus::CanAuthorWith<Block>,
			>,
			sc_finality_grandpa::LinkHalf<Block, FullClient, FullSelectChain>,
		),
	>,
	ServiceError,
> {
	let inherent_data_providers = build_inherent_data_providers()?;

	let (client, backend, keystore_container, task_manager) =
		sc_service::new_full_parts::<Block, RuntimeApi, Executor>(&config)?;
	let client = Arc::new(client);

	let select_chain = sc_consensus::LongestChain::new(backend.clone());

	let transaction_pool = sc_transaction_pool::BasicPool::new_full(
		config.transaction_pool.clone(),
		config.role.is_authority().into(),
		config.prometheus_registry(),
		task_manager.spawn_handle(),
		client.clone(),
	);

	let (grandpa_block_import, grandpa_link) = sc_finality_grandpa::block_import(
		client.clone(),
		&(client.clone() as std::sync::Arc<_>),
		select_chain.clone(),
	)?;

	let can_author_with = sp_consensus::CanAuthorWithNativeVersion::new(client.executor().clone());

	let pow_block_import = sc_consensus_poscan::PowBlockImport::new(
		grandpa_block_import,
		client.clone(),
		sha3pow::PoscanAlgorithm, //::new(client.clone()),
		0, // check inherents starting at block 0
		select_chain.clone(),
		inherent_data_providers.clone(),
		can_author_with,
	);

	let import_queue = sc_consensus_poscan::import_queue(
		Box::new(pow_block_import.clone()),
		None,
		sha3pow::PoscanAlgorithm, //::new(client.clone()),
		inherent_data_providers.clone(),
		&task_manager.spawn_handle(),
		config.prometheus_registry(),
	)?;

	Ok(PartialComponents {
		client,
		backend,
		import_queue,
		keystore_container,
		task_manager,
		transaction_pool,
		select_chain,
		inherent_data_providers,
		other: (pow_block_import, grandpa_link),
	})
}

// use sha3pow::Module;
// use sha3pow::TT;
//pub trait Trait: sha3pow::Trait {}


// use frame_support::{decl_module, decl_storage};
// // use codec::{Decode, Encode};
//
// // }
//
// // #[derive(Encode, Decode, Default)] // , RuntimeDebug)]
// // pub struct Obj3d<Hash, Obj> {
// // 	pub hash: Hash,
// // 	pub obj: Obj,
//
// pub trait Config: frame_system::Config {}
//
// decl_storage! {
//     trait Store for Module<T: Config> as ObjModule {
//         /// The storage item for our proofs.
// 		/// It maps a proof to the user who made the claim and when they made it.
//         // Objs: map hasher(blake2_128_concat) Vec<u8> => (T::AccountId, T::BlockNumber);
//         Objs: map hasher(blake2_128_concat) U256 => Vec<u8>;
//     }
// }
//
// decl_module! {
// 		pub struct Module<T: Config> for enum Call where origin: T::Origin {
// 		}
// 	}



/// Builds a new service for a full client.
pub fn new_full(config: Configuration) -> Result<TaskManager, ServiceError> {
	let sc_service::PartialComponents {
		client,
		backend,
		mut task_manager,
		import_queue,
		keystore_container,
		select_chain,
		transaction_pool,
		inherent_data_providers,
		other: (pow_block_import, grandpa_link),
	} = new_partial(&config)?;

	let (network, network_status_sinks, system_rpc_tx, network_starter) =
		sc_service::build_network(sc_service::BuildNetworkParams {
			config: &config,
			client: client.clone(),
			transaction_pool: transaction_pool.clone(),
			spawn_handle: task_manager.spawn_handle(),
			import_queue,
			on_demand: None,
			block_announce_validator_builder: None,
		})?;

	if config.offchain_worker.enabled {
		sc_service::build_offchain_workers(
			&config,
			backend.clone(),
			task_manager.spawn_handle(),
			client.clone(),
			network.clone(),
		);
	}

	let is_authority = config.role.is_authority();
	let prometheus_registry = config.prometheus_registry().cloned();
	let enable_grandpa = !config.disable_grandpa;

	let (_rpc_handlers, telemetry_connection_notifier) =
		sc_service::spawn_tasks(sc_service::SpawnTasksParams {
			network: network.clone(),
			client: client.clone(),
			keystore: keystore_container.sync_keystore(),
			task_manager: &mut task_manager,
			transaction_pool: transaction_pool.clone(),
			rpc_extensions_builder: Box::new(|_, _| ()),
			on_demand: None,
			remote_blockchain: None,
			backend,
			network_status_sinks,
			system_rpc_tx,
			config,
		})?;

	if is_authority {
		let proposer = sc_basic_authorship::ProposerFactory::new(
			task_manager.spawn_handle(),
			client.clone(),
			transaction_pool,
			prometheus_registry.as_ref(),
		);

		let can_author_with =
			sp_consensus::CanAuthorWithNativeVersion::new(client.executor().clone());

		// Parameter details:
		//   https://substrate.dev/rustdocs/v3.0.0/sc_consensus_pow/fn.start_mining_worker.html
		// Also refer to kulupu config:
		//   https://github.com/kulupu/kulupu/blob/master/src/service.rs
		let (_worker, worker_task) = sc_consensus_poscan::start_mining_worker(
			Box::new(pow_block_import),
			client,
			select_chain,
			PoscanAlgorithm, // ::new(client.clone()),
			proposer,
			network.clone(),
			None,
			inherent_data_providers,
			// time to wait for a new block before starting to mine a new one
			Duration::from_secs(10),
			// how long to take to actually build the block (i.e. executing extrinsics)
			Duration::from_secs(10),
			can_author_with,
		);

		task_manager
			.spawn_essential_handle()
			.spawn_blocking("pow", worker_task);

		// Start Mining
		// let mut nonce: U256 = U256::from(0);
		let	mut poscan_data: Option<PoscanData> = None;
		let mut poscan_hash: Option<H256> = None;

		thread::spawn(move || loop {
			let worker = _worker.clone();
			let metadata = worker.lock().metadata();
			if let Some(mut metadata) = metadata {
				let mut ps: H256;
				let ps = match poscan_hash {
					Some(ps) => ps,
					None => continue,
				};
				let compute = Compute {
					difficulty: metadata.difficulty,
					pre_hash: metadata.pre_hash,
					poscan_hash: ps,
				};
				let seal = compute.compute();
				if hash_meets_difficulty(&seal.work, seal.difficulty) {
					// nonce = U256::from(0);
					let mut worker = worker.lock();

					// let v = vec![1, 2, 3];
					// metadata.obj = v;
					// // Objs::insert(nonce, v);
					// <Module<Configuration>>::put_obj(&nonce, v);

					// let b = BlockId::Hash(parent_hash);
					// let at = runtime::block_num();

					// let at = client.runtime_api();
					// let _i = client.runtime_api().get_obj(1);

					if let Some(ref psdata) = poscan_data {
						// let _ = psdata.encode();
						worker.submit(seal.encode(), &psdata);
					}
				} else {
					// nonce = nonce.saturating_add(U256::from(1));
					// if nonce == U256::MAX {
					// 	nonce = U256::from(0);
					// }

					use pallet_poscan::DEQUE;
					let mut lock = DEQUE.lock();
					let maybe_mining_prop = (*lock).pop_front();
					if let Some(mp) = maybe_mining_prop {
						let hashes = get_obj_hashes(&mp.pre_obj);
						if hashes.len() > 0 {
							let obj_hash = hashes[0];
							let dh = DoubleHash { pre_hash: metadata.pre_hash, obj_hash };
							poscan_hash = Some(dh.calc_hash());
							poscan_data = Some(PoscanData { hashes, obj: mp.pre_obj });
						}
					}
				}
			} else {
				thread::sleep(Duration::new(1, 0));
			}
		});
	}

	let grandpa_config = sc_finality_grandpa::Config {
		gossip_duration: Duration::from_millis(333),
		justification_period: 512,
		name: None,
		observer_enabled: false,
		keystore: Some(keystore_container.sync_keystore()),
		is_authority,
	};

	if enable_grandpa {
		// start the full GRANDPA voter
		// NOTE: non-authorities could run the GRANDPA observer protocol, but at
		// this point the full voter should provide better guarantees of block
		// and vote data availability than the observer. The observer has not
		// been tested extensively yet and having most nodes in a network run it
		// could lead to finality stalls.
		let grandpa_config = sc_finality_grandpa::GrandpaParams {
			config: grandpa_config,
			link: grandpa_link,
			network,
			telemetry_on_connect: telemetry_connection_notifier.map(|x| x.on_connect_stream()),
			voting_rule: sc_finality_grandpa::VotingRulesBuilder::default().build(),
			prometheus_registry,
			shared_voter_state: sc_finality_grandpa::SharedVoterState::empty(),
		};

		// the GRANDPA voter task is considered infallible, i.e.
		// if it fails we take down the service with it.
		task_manager.spawn_essential_handle().spawn_blocking(
			"grandpa-voter",
			sc_finality_grandpa::run_grandpa_voter(grandpa_config)?,
		);
	}

	network_starter.start_network();
	Ok(task_manager)
}

/// Builds a new service for a light client.
pub fn new_light(config: Configuration) -> Result<TaskManager, ServiceError> {
	let inherent_data_providers = build_inherent_data_providers()?;

	let (client, backend, keystore_container, mut task_manager, on_demand) =
		sc_service::new_light_parts::<Block, RuntimeApi, Executor>(&config)?;

	let transaction_pool = Arc::new(sc_transaction_pool::BasicPool::new_light(
		config.transaction_pool.clone(),
		config.prometheus_registry(),
		task_manager.spawn_handle(),
		client.clone(),
		on_demand.clone(),
	));

	let select_chain = sc_consensus::LongestChain::new(backend.clone());

	let (grandpa_block_import, _) = sc_finality_grandpa::block_import(
		client.clone(),
		&(client.clone() as Arc<_>),
		select_chain.clone(),
	)?;

	// FixMe #375
	let _can_author_with = sp_consensus::CanAuthorWithNativeVersion::new(client.executor().clone());

	let pow_block_import = sc_consensus_poscan::PowBlockImport::new(
		grandpa_block_import,
		client.clone(),
		PoscanAlgorithm, //::new(client.clone()),
		0, // check inherents starting at block 0
		select_chain,
		inherent_data_providers.clone(),
		sp_consensus::AlwaysCanAuthor,
	);

	let import_queue = sc_consensus_poscan::import_queue(
		Box::new(pow_block_import),
		None,
		PoscanAlgorithm, // ::new(client.clone()),
		inherent_data_providers,
		&task_manager.spawn_handle(),
		config.prometheus_registry(),
	)?;

	let (network, network_status_sinks, system_rpc_tx, network_starter) =
		sc_service::build_network(sc_service::BuildNetworkParams {
			config: &config,
			client: client.clone(),
			transaction_pool: transaction_pool.clone(),
			spawn_handle: task_manager.spawn_handle(),
			import_queue,
			on_demand: Some(on_demand.clone()),
			block_announce_validator_builder: None,
		})?;

	if config.offchain_worker.enabled {
		sc_service::build_offchain_workers(
			&config,
			backend.clone(),
			task_manager.spawn_handle(),
			client.clone(),
			network.clone(),
		);
	}

	sc_service::spawn_tasks(sc_service::SpawnTasksParams {
		remote_blockchain: Some(backend.remote_blockchain()),
		transaction_pool,
		task_manager: &mut task_manager,
		on_demand: Some(on_demand),
		rpc_extensions_builder: Box::new(|_, _| ()),
		config,
		client,
		keystore: keystore_container.sync_keystore(),
		backend,
		network,
		network_status_sinks,
		system_rpc_tx,
	})?;

	network_starter.start_network();

	Ok(task_manager)
}

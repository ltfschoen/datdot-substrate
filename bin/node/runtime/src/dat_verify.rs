#![cfg_attr(not(feature = "std"), no_std)]
/// A runtime module template with necessary imports

/// Feel free to remove or edit this file as needed.
/// If you change the name of this file, make sure to update its references in runtime/src/lib.rs
/// If you remove this file, you can remove those references


/// For more guidance on Substrate modules, see the example module
/// https://github.com/paritytech/substrate/blob/master/srml/example/src/lib.rs
use sp_std::prelude::*;
use sp_std::fmt::Debug;
use frame_support::{
	decl_module,
	decl_storage, 
	decl_event,
	decl_error,
	debug::native,
	dispatch,
	ensure,
	fail,
	StorageValue,
	StorageMap,
	Parameter,
	traits::{
		Randomness,
		ChangeMembers,
	},
};
use sp_std::convert::{
	TryInto, TryFrom
};
use frame_system::{
	self as system,
	ensure_signed,
	ensure_root,
	offchain
};
use codec::{Encode, Decode, Output};
use sp_core::{
	ed25519,
	Hasher,
	Blake2Hasher, 
	H256,
	H512,
	convert_hash,
};
use core::mem;
use sp_runtime::{
	RuntimeDebug,
	traits::{
		Verify,
		CheckEqual,
		Dispatchable,
		StaticLookup,
		EnsureOrigin,
		SimpleBitOps,
		MaybeDisplay,
		MaybeSerializeDeserialize,
		Member
	},
	transaction_validity::{
		TransactionValidity,
		TransactionLongevity,
		ValidTransaction,
		InvalidTransaction
	}
};

pub type Public = ed25519::Public;
pub type Signature = ed25519::Signature;

/// The module's configuration trait.
pub trait Trait: system::Trait{
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
	type Hash: 
	Parameter + Member + MaybeSerializeDeserialize + Debug + MaybeDisplay + SimpleBitOps
	+ Default + Copy + CheckEqual + sp_std::hash::Hash + AsRef<[u8]> + AsMut<[u8]>;
	type Randomness: Randomness<<Self as system::Trait>::Hash>;
	type ForceOrigin: EnsureOrigin<<Self as system::Trait>::Origin>;
	type SeederMembership: ChangeMembers<<Self as system::Trait>::AccountId>;
	type UserMembership: ChangeMembers<<Self as system::Trait>::AccountId>;
	type Proposal: Parameter + Dispatchable<Origin=Self::Origin>;
}

#[derive(Decode, PartialEq, Eq, Encode, Clone, RuntimeDebug)]
pub struct Node {
	index: u64,
	hash: H256,
	size: u64
}

impl Node {
	fn height(&self) -> u64 {
		Self::get_height(self.index)
	}

	fn get_height(index : u64) -> u64 {
		let mut bit_pointer : u64 = 1;
		let mut cur_height : u64 = 0;
		while (index | bit_pointer) == index {
			cur_height += 1;
			bit_pointer *= 2;
		}
		cur_height
	}

	//get the index as if it were a leaf
	fn relative_index(&self) -> u64 {
		Self::index_at_height(self.index, self.height())
	}

	fn index_at_height(index : u64, height : u64) -> u64 {
		index / 2u64.pow(height.try_into().unwrap())
	}

	//get the last index at a given height, given a max leaf index
	fn top_index_at_height(height : u64, max_index : u64) -> Option<u64> {
		let offset = 2u64.pow(height.try_into().unwrap())-1;
		let interval = 2u64.pow((height+1).try_into().unwrap());
		let mut furthest_leaf = 0;
		let mut result = None;
		if height == 0 {
			result = Some(max_index);
		} else {
			for i in 0..height { //not inclusive of height intended
				furthest_leaf += 2u64.pow(i.try_into().unwrap());
			}
		}
		if max_index > offset {
			let mut next_result = true;
			while next_result {	
				match result {
					Some(index) => {
						if index + interval + furthest_leaf > max_index {
							next_result = false;
						} else {
							result = Some(index+interval);
						}
					},
					None => {
						result = Some(offset); 
					},
				}
			}
		}
		result
	}

	//get the highest height, given a max leaf index
	fn highest_at_index(max_index : u64) -> u64 {
		//max_index == 2^n - 2
		//return n
		//TODO: needs tests/audit, this was written using trail and error.
		let mut current_index = Some(max_index);
		let mut current_height : u64 = 0;
		while current_index.unwrap_or(0) > 2u64.pow(current_height.try_into().unwrap()) {
			current_index = current_index
				.unwrap_or(0)
				.checked_sub(2u64.pow((current_height+1).try_into().unwrap()));
			match current_index {
				Some(_) => current_height += 1,
				None => (),
			}
		}
		current_height
	}


	//get indexes of nodes in a merkle tree that are used to
	//calculate the root hash
	fn get_orphan_indeces(highest_index : u64) -> Vec<u64> {
		let mut indeces : Vec<u64> = Vec::new();
		for i in 0..Self::highest_at_index(highest_index)+1 {
			match Self::top_index_at_height(i, highest_index) {
				Some(expr) => if expr % 2 == 0 {
					indeces.push(expr);
				},
				None => (),
			}
		}
		indeces
	}

	fn is_orphan(&self, highest_index : u64) -> bool {
		Self::get_orphan_indeces(highest_index)
			.contains(&self.index)
	}
}

#[derive(Decode, PartialEq, Eq, Encode, Clone, RuntimeDebug)]
pub struct Proof {
	index: u64,
	nodes: Vec<Node>,
	signature: Option<Signature>
}


//https://datprotocol.github.io/how-dat-works/#hashes-and-signatures
#[derive(Decode, PartialEq, Eq, Encode, Clone, RuntimeDebug)]
pub struct ChunkHashPayload {
	hash_type: u8, //0
	chunk_length: u64,
	chunk_content: Vec<u8>
}


#[derive(Decode, PartialEq, Eq, Encode, Clone, RuntimeDebug)]
pub struct ParentHashPayload {
	hash_type: u8, //1
	total_length: u64,
	child_hashes: [H256; 2]
}


#[derive(Decode, PartialEq, Eq, Encode, Clone, Copy, RuntimeDebug)]
pub struct ParentHashInRoot {
	hash: H256,
	hash_number: u64,
	total_length: u64
}


#[derive(Decode, PartialEq, Eq, Encode, Clone, RuntimeDebug)]
pub struct RootHashPayload {
	hash_type: u8, //2
	children: Vec<ParentHashInRoot>
}

trait HashPayload where Self: Sized + Encode {
	fn hash(&self) -> H256 {
		self.using_encoded(|b|{
			native::info!("Hash Payload: {:x?}", b);
			Blake2Hasher::hash(b)
		})
	}
}

impl HashPayload for RootHashPayload {
	//hackish manual manipulation of the bits being hashed.
	//made using trial-and-error. lots of memcpy. loop. kill it with fire.
	fn hash(&self) -> H256 {
		self.using_encoded(|dirtybits|{
			native::info!("Root Hash Dirty Payload [{:#?}]: {:x?}", dirtybits.len(), dirtybits);
			let mut dirtycopy = dirtybits.to_vec();
			let mut x = 2;
			let mut bits : Vec<u8> = Vec::new();
			bits.push(*dirtycopy.get(0).expect(""));
			loop{
				let pt1 = dirtycopy.get(x+0..x+32).expect("");
				let mut pt2: &mut [u8; 8] = &mut [0,0,0,0,0,0,0,0];
				pt2.copy_from_slice(dirtycopy.get(x+32..x+40).expect(""));
				pt2.reverse();
				let mut pt3: &mut [u8;8] = &mut [0,0,0,0,0,0,0,0];
				pt3.copy_from_slice(dirtycopy.get(x+40..x+48).expect(""));
				pt3.reverse();
				let mut newbits = [
					pt1,
					pt2,
					pt3
				].concat();
				x += 48;
				bits.extend_from_slice(&mut newbits);
				match dirtybits.get(x) {
					Some(_) => (),
					None => break,
				}
			};
			native::info!("Root Hash Clean Payload [{:#?}]: {:x?}", bits.len(), bits);
			Blake2Hasher::hash(bits.as_slice())
		})
	}
}
impl HashPayload for ParentHashPayload {
	fn hash(&self) -> H256 {
		self.using_encoded(|b|{
			native::info!("Parent Hash Payload Dirty [{:x?}]: {:x?}", b.len(), b);
			let mut dirtycopy = b.to_vec();
			let mut cleanbits: Vec<u8> = Vec::new();
			let mut hash_type = dirtycopy.get(0..1).expect("");
			let mut total_length = &mut [0,0,0,0,0,0,0,0];
			total_length.copy_from_slice(dirtycopy.get(1..9).expect(""));
			total_length.reverse();

			native::info!("Child nodes raw: {:x?}", dirtycopy.get(9..).expect(""));
			cleanbits.extend_from_slice(hash_type);
			cleanbits.extend_from_slice(total_length);
			native::info!("Parent Hash Payload [{:x?}]: {:x?}", b.len(), b);
			Blake2Hasher::hash(cleanbits.as_slice())
		})
	}
}
impl HashPayload for ChunkHashPayload {
	fn hash(&self) -> H256 {
		self.using_encoded(|b|{
			native::info!("Chunk Hash Payload: {:x?}", b);
			let mut dirtycopy = b.to_vec();
			let mut cleanbits: Vec<u8> = Vec::new();
			let mut hash_type = dirtycopy.get(0..1).expect("");
			let mut total_length = &mut [0,0,0,0,0,0,0,0];
			total_length.copy_from_slice(dirtycopy.get(1..9).expect(""));
			total_length.reverse();

			native::info!("Child nodes raw: {:x?}", dirtycopy.get(9..).expect(""));
			cleanbits.extend_from_slice(hash_type);
			cleanbits.extend_from_slice(total_length);
			Blake2Hasher::hash(cleanbits.as_slice())
		})
	}
}

#[derive(Decode, PartialEq, Eq, Encode, Clone, RuntimeDebug)]
pub struct Attestation {
	//todo, actually decide format
	location: u8,
	latency: Option<u8> //may be failure.
}

type DatIdIndex = u64;
type DatIdVec = Vec<DatIdIndex>;
type UserIdIndex = u64; 
type DatSize = u64;

decl_event!(
	pub enum Event<T> 
	where
	AccountId = <T as system::Trait>::AccountId,
	BlockNumber = <T as system::Trait>::BlockNumber 
	{
		SomethingStored(DatIdIndex, Public),
		SomethingUnstored(DatIdIndex, Public),
		Challenge(AccountId,BlockNumber),
		ChallengeFailed(AccountId, Public),
		NewPin(AccountId, Public),
		Attest(AccountId, Attestation),
	}
);

decl_error! {
    pub enum Error for Module<T: Trait> {
		PermissionError,
		UnsignedProof,
		VerificationFailed,
		ProvesWrongChunk,
		MissingLeaf,
		ChunkHashVerificationFailed,
		RootHashVerificationFailed,
		InvalidState,
		InvalidTreeSize
    }
}


// Dat related storage items.
decl_storage! {
	trait Store for Module<T: Trait> as DatVerify {
		// A vec of free indeces, with the last item usable for `len`
		pub DatId get(next_id): DatIdVec;
		// Each dat archive has a public key
		pub DatKey get(public_key): map hasher(twox_256) DatIdIndex => Public;
		// Each dat archive has a tree size
		// TODO: remove calls to this when expecting indeces
		pub TreeSize get(tree_size): map hasher(blake2_256) Public => DatSize;
		// each dat archive has a merkle root
		pub MerkleRoot get(merkle_root): map hasher(blake2_256) Public => (H256, Signature);
		// vec of occupied user indeces for when users are removed.
		pub UsersCount: Vec<u64>;
		// users are put into an "array"
		pub Users get(user): linked_map hasher(twox_256) UserIdIndex => T::AccountId;
		// each user has a vec of dats they seed
		pub UsersStorage: map hasher(blake2_256) T::AccountId => Vec<DatIdIndex>;
		// each dat has a vec of users pinning it
		pub DatHosters: map hasher(blake2_256) Public => Vec<T::AccountId>;
		// each user has a mapping and vec of dats they want seeded
		pub UserRequestsMap: map hasher(blake2_256) Public => T::AccountId;

		// current check condition
		pub ChallengeIndex: u64;
		pub UserIndex: u64;
		// Challenge => User
		pub ChallengeMap: linked_map hasher(twox_256) u64 => u64;
		// Dat and which index to verify
		pub SelectedChallenges: map hasher(twox_256) u64 => (Public, u64, T::BlockNumber);
		pub RemovedDats: Vec<Public>;
		pub SelectedUsers: map hasher(twox_256) u64 => T::AccountId;
		// (index, challenge count)
		pub SelectedUserIndex: map hasher(twox_256) T::AccountId => (u64, u64);
		pub Nonce: u64;

		// attestor => relevant challenge
		// attestors and attestations are ephemeral
		pub Attestors: map hasher(twox_256) T::AccountId => u64;
		// challenge => ([expected attestors], [rewarded friends], [seen attestations])
		pub ChallengeAttestations: map hasher(twox_256) u64 =>(
			Vec<T::AccountId>,
			Vec<T::AccountId>,
			Vec<(T::AccountId, Attestation)>
		);
	}
}

decl_module!{
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		fn deposit_event() = default;
		type Error = Error<T>;
		
		fn on_initialize(n: T::BlockNumber) {
			let dat_vec : Vec<DatIdIndex> = <DatId>::get();
			let challenge_index = <ChallengeIndex>::get();

			match dat_vec.last() {
				Some(last_index) => {
			// if no one is currently selected to give proof, select someone
			if !<ChallengeMap>::exists(&challenge_index) && <UsersCount>::exists() {
				let nonce = <Nonce>::get();
				let new_random = (T::Randomness::random(b"dat_verify_init"), nonce)
				.using_encoded(|b| Blake2Hasher::hash(b))
				.using_encoded(|mut b| u64::decode(&mut b))
				.expect("hash must be of correct size; Qed");
				let new_time_limit = new_random % last_index;
				let challenge_length = new_time_limit.try_into().unwrap_or(2) + 1;
				let future_block = 
					n + T::BlockNumber::from(challenge_length);
				let valid_users = <UsersCount>::get();
				let valid_users_len = valid_users.len();
				let selected_user = (new_random as usize % valid_users_len);
				let random_user_index = valid_users.get(selected_user)
					.expect("the remainder is always in bounds when % len");
				let random_user = <Users<T>>::get(random_user_index);
				let users_dats = <UsersStorage<T>>::get(&random_user);
				let users_dats_len = users_dats.len();
				let random_dat_id = users_dats.get(new_random as usize % users_dats_len)
					.expect("the remainder is always in bounds when % len");
				let random_dat = <DatKey>::get(random_dat_id);
				let dat_tree_len = <TreeSize>::get(&random_dat);
				let mut random_leave = 0;
				if dat_tree_len != 0 { // avoid 0 divisor 
					random_leave = new_random % dat_tree_len;
				} 
				let y : u64;
				if !<SelectedUserIndex<T>>::exists(&random_user) {
					let user_index = <UserIndex>::get();
					<SelectedUserIndex<T>>::insert(&random_user, (user_index, 1));
					<UserIndex>::put(<UserIndex>::get() + 1);
					y = user_index;
				} else {
					let (user_index, count) = <SelectedUserIndex<T>>::get(&random_user);
					<SelectedUserIndex<T>>::insert(&random_user, (user_index, count+1));
					y = user_index;
				}
				<SelectedChallenges<T>>::insert(&challenge_index, (random_dat, random_leave, future_block));
				<SelectedUsers<T>>::insert(&y, &random_user);
				<ChallengeMap>::insert(challenge_index, y);
				<Nonce>::put(<Nonce>::get() + 1);
				<ChallengeIndex>::put(<ChallengeIndex>::get() + 1);
				Self::deposit_event(RawEvent::Challenge(random_user, future_block));
			}},
				None => (),
			}
		}

		//TODO: submit attestation that a peer is online and behaving correctly on the dat network
		fn submit_attestation(origin, attestation: Attestation) {
			let attestor = ensure_signed(origin)?;
			// TODO: verify you have been requested an attestation
			// TODO: remove challenge iff threshold is met
			Self::deposit_event(RawEvent::Attest(attestor, attestation));
		}

		
		fn force_clear_challenge(origin, account: T::AccountId, challenge_index: u64){
			T::ForceOrigin::try_origin(origin)
				.map(|_| ())
				.or_else(ensure_root)?; 
			let account_index = <ChallengeMap>::get(&challenge_index);
			let (_, count) = <SelectedUserIndex<T>>::get(&account);
			match count {
				1 => {	
					<SelectedUsers<T>>::remove(account_index);
					<SelectedUserIndex<T>>::remove(account);
				},
				_ => <SelectedUserIndex<T>>::insert(account, (account_index, count-1))
			}
			<SelectedChallenges<T>>::remove(challenge_index);
			<ChallengeMap>::remove(challenge_index);
		}
		
		
		//test things progressively, doing quicker computations first.
		fn submit_proof(origin, challenge_index: u64, proof: Proof, unsigned_root_hash: H256, chunk_content: Vec<u8>) {
			let account = ensure_signed(origin)?;
			let account_index = <ChallengeMap>::get(&challenge_index);
			ensure!(
				account == <SelectedUsers<T>>::get(&account_index),
				Error::<T>::PermissionError
			);
			let index_proved = proof.index;
			let challenge = <SelectedChallenges<T>>::get(&challenge_index);
			let signature_option = proof.signature;
			ensure!(
				match signature_option {
					Some(signature) => {
						ensure!(
							&signature.verify(
								unsigned_root_hash.as_bytes(),
								&challenge.0
							),
							Error::<T>::VerificationFailed
						);
						true
					},
					None => false
				},
				Error::<T>::UnsignedProof
			);
			ensure!(
				index_proved == challenge.1,
				Error::<T>::ProvesWrongChunk
			);
			let payload = ChunkHashPayload{
				hash_type : 0,
				chunk_length : chunk_content.len() as u64,
				chunk_content : chunk_content
			};
			let chunk_hash = payload.hash();
			let proof_nodes : Vec<Node> = proof.nodes.clone();
			let node_indeces : Vec<u64> = proof_nodes.clone().into_iter().map(|m| {
					m.index
				}).collect();
			let leaf_node : Option<&Node> = match node_indeces.binary_search(&index_proved) {
				Ok(leaf_node_index) => proof_nodes.get(leaf_node_index),
				Err(_) => None
			};
			match leaf_node {
				Some(node) => ensure!(
					chunk_hash == node.hash,
					Error::<T>::ChunkHashVerificationFailed
				),
				None => ensure!(
					false,
					Error::<T>::ChunkHashVerificationFailed
				)
			}
			let root_indeces = Node::get_orphan_indeces(index_proved);
			let mut root_nodes : Vec<ParentHashInRoot> = Vec::new();
			proof_nodes.iter().for_each(|check : &Node| {
				let node_index = check.index; 
				if root_indeces.contains(&node_index) {
					let orphan_hash = ParentHashInRoot {
						hash: check.hash,
						hash_number: check.index,
						total_length: check.size
					};
					root_nodes.push(orphan_hash);
				}
			});
			root_nodes.sort_unstable_by(|a , b|
				a.hash_number.cmp(&b.hash_number)
			);
			let root_hash_payload = RootHashPayload {
				hash_type: 2,
				children: root_nodes
			};
			let root_hash = root_hash_payload.hash();
			ensure!(
				root_hash == unsigned_root_hash,
				Error::<T>::RootHashVerificationFailed
			);
			let temporary_root = system::RawOrigin::Root;
			match Self::force_clear_challenge(temporary_root.into(), account, challenge_index) {
				Ok(x) => x,
				Err(x) => fail!(x),
			}
			// else let the user try again until time limit
		}

		// Submit or update a piece of data that you want to have users copy, optionally provide chunk for execution.
		fn register_data(origin, merkle_root: (Public, RootHashPayload, H512)) {
			let account = ensure_signed(origin)?;
			let pubkey = merkle_root.0;
			let root_hash_children = merkle_root.clone().1.children;
			let root_hash_sanitized_payload = RootHashPayload {
				hash_type: 2,
				children: root_hash_children,
			};
			let root_hash = root_hash_sanitized_payload.hash();
			let sig = Signature::from_h512(merkle_root.2);
			native::info!("Register Data Merkle Root: {:x?}", merkle_root);
			native::info!("Register Data Merkle Root Hash: {:x?}", root_hash);
			//FIXME: we don't currently verify if we are updating to a newer root from an older one.
			//verify the signature
			ensure!(
				sig.verify(
					root_hash.as_bytes(),
					&pubkey
				),
				Error::<T>::VerificationFailed
			);
			let temporary_root = system::RawOrigin::Root;
			// the rest of the logic is already in force_register_data so, just call that function.
			match Self::force_register_data(temporary_root.into(), account, merkle_root) {
				Ok(x) => x,
				Err(x) => fail!(x),
			}
		}

		//debug method when you don't have valid data for register_data, no validity checks, only root.
		fn force_register_data(
			origin,
			account: T::AccountId,
			merkle_root: (Public, RootHashPayload, H512)
		)
		{
			T::ForceOrigin::try_origin(origin)
				.map(|_| ())
				.or_else(ensure_root)?;
			let pubkey = merkle_root.0;
			let sig = Signature::from_h512(merkle_root.2);
			let mut lowest_free_index : DatIdIndex = 0;
			let mut tree_size : u64 = u64::min_value();
			let root_hash = merkle_root.1.hash(); //todo: do not calculate twice!
			for child in merkle_root.1.children {
				tree_size += child.total_length;
			}
			ensure!(
				tree_size >= 1,
				Error::<T>::InvalidTreeSize
			);
			let mut dat_vec : Vec<DatIdIndex> = <DatId>::get();
			if !<MerkleRoot>::exists(&pubkey){
				match dat_vec.first() {
					Some(_) => {
						dat_vec.sort_unstable();
						lowest_free_index = dat_vec.remove(0);
						dat_vec.push(lowest_free_index + 1);
						dat_vec.sort_unstable();
						dat_vec.dedup();
					},
					None => {
						//add an element if the vec is empty
						dat_vec.push(1);
						lowest_free_index = 0;
					},
				}
				//register new unknown dats
				<DatKey>::insert(&lowest_free_index, &pubkey)
			}
			<MerkleRoot>::insert(&pubkey, (root_hash, sig));
			<DatId>::put(dat_vec);
			<TreeSize>::insert(&pubkey, tree_size);
			<UserRequestsMap<T>>::insert(&pubkey, &account);
			Self::deposit_event(RawEvent::SomethingStored(lowest_free_index, pubkey));
		}

		//user stops requesting others pin their data
		fn unregister_data(origin, index: DatIdIndex){
			let account = ensure_signed(origin)?;
			let pubkey = <DatKey>::get(index);
			//only allow owner to unregister
			ensure!(
				<UserRequestsMap<T>>::get(&pubkey) == account,
				Error::<T>::PermissionError
			);
			let mut dat_vec = <DatId>::get();
			match dat_vec.last() {
				Some(last_index) => {
					if index == last_index - 1 {
						dat_vec.pop();
					}
					dat_vec.push(index);
					dat_vec.sort_unstable();
					dat_vec.dedup();
					<DatId>::put(dat_vec);
				},
				None => (),
			}
			<DatHosters<T>>::get(&pubkey)
				.iter()
				.for_each(|account| {
					let mut storage : Vec<DatIdIndex> =
						<UsersStorage<T>>::get(account.clone());
					match storage.binary_search(&index).ok() {
						Some(index) => {storage.remove(index);},
						None => (),
					}
					<UsersStorage<T>>::insert(account.clone(), &storage);
				});
			<DatHosters<T>>::remove(&pubkey);
			<DatKey>::remove(&index);
			<UserRequestsMap<T>>::remove(&pubkey);
			<TreeSize>::remove(&pubkey);
			<MerkleRoot>::remove(&pubkey);
			//if the dat being unregistered is currently part of the challenge
			let mut tmp : Vec<Public> = Vec::new();
			if <RemovedDats>::exists() {
				tmp = <RemovedDats>::get();	
			}
			tmp.push(pubkey);
			<RemovedDats>::put(tmp);
			Self::deposit_event(RawEvent::SomethingUnstored(index, pubkey));
		}

		// User requests a dat for them to pin. FIXME: May return a dat they are already pinning.
		fn register_seeder(origin) {
			//TODO: bias towards unseeded dats and high incentive
			let account = ensure_signed(origin)?;
			let dat_vec = <DatId>::get();
			match dat_vec.last() {
				Some(last_index) => {
				let nonce = <Nonce>::get();
				let new_random = (T::Randomness::random(b"dat_verify_register"), &nonce, &account)
					.using_encoded(|b| Blake2Hasher::hash(b))
					.using_encoded(|mut b| u64::decode(&mut b))
					.expect("hash must be of correct size; Qed");
				let random_index = new_random % last_index;
				let dat_pubkey = DatKey::get(&random_index);
				let mut current_user_dats = <UsersStorage<T>>::get(&account);
				let mut dat_hosters = <DatHosters<T>>::get(&dat_pubkey);
				current_user_dats.push(random_index);
				current_user_dats.sort_unstable();
				current_user_dats.dedup();
				dat_hosters.push(account.clone());
				dat_hosters.sort_unstable();
				dat_hosters.dedup();
				<DatHosters<T>>::insert(&dat_pubkey, &dat_hosters);
				<UsersStorage<T>>::insert(&account, &current_user_dats);
				<Nonce>::mutate(|m| *m += 1);
				if(current_user_dats.len() == 1){
					let user_index_option = <UsersCount>::get().pop();
					let current_user_index = match user_index_option {
						Some(x) => x,
						None => 0,
					};
					match current_user_index.checked_add(1){
						Some(i) => {
							<Users<T>>::insert(&i, &account);
							let mut users = <UsersCount>::get();
							users.push(i);
							<UsersCount>::put(users);
						},
						None => (),
					}
				}
				Self::deposit_event(RawEvent::NewPin(account, dat_pubkey));
				},
				None => (),
			}
		} 

		fn unregister_seeder(origin) {
			let account = ensure_signed(origin)?;
			for dat_id in <UsersStorage<T>>::get(&account) {
				let dat_key = <DatKey>::get(dat_id);
				let mut hosters = <DatHosters<T>>::get(&dat_key);
				hosters.sort_unstable();
				match hosters.binary_search(&account) {
					Ok(index) => {
						hosters.remove(index);
					},
					_ => (),
				}
				<DatHosters<T>>::insert(dat_key, hosters);
			}
			for (user_index, user_account) in <Users<T>>::enumerate(){
				if user_account == account {
					<Users<T>>::remove(user_index);
					let mut user_indexes = <UsersCount>::get();
					match user_indexes.binary_search(&user_index){
						Ok(i) => {
							user_indexes.remove(i);
						},
						_ => (),
					}
					if user_indexes.len() > 0 {
						<UsersCount>::put(user_indexes);
					} else {
						<UsersCount>::kill();
					}
				}
			}
			<UsersStorage<T>>::remove(account);
		}

		fn punish_seeder(origin, punished: T::AccountId) {
			ensure_root(origin)?;
			// todo: punish seeder.
		}


		fn reward_seeder(origin, rewarded: T::AccountId) {
			ensure_root(origin)?;
			// todo: punish seeder.
		}

		//TODO: this is probably bad and should probably go into an offchain worker.
		fn on_finalize(n: T::BlockNumber) {
			for (challenge_index, user_index) in <ChallengeMap>::enumerate() {
				let user = <SelectedUsers<T>>::get(user_index);
				let dat = <SelectedChallenges<T>>::get(challenge_index).0;
				let time = <SelectedChallenges<T>>::get(challenge_index).2;
				let temporary_root = system::RawOrigin::Root;
				let inner_origin =
					system::RawOrigin::Signed(user.clone());
				if (n == time) {
					<SelectedUsers<T>>::remove(user_index);
					<SelectedUserIndex<T>>::remove(&user);
					<SelectedChallenges<T>>::remove(challenge_index);
					<ChallengeMap>::remove(challenge_index);
					Self::punish_seeder(temporary_root.into(), user.clone());
					Self::unregister_seeder(inner_origin.into());
					Self::deposit_event(RawEvent::ChallengeFailed(user, dat));
				} else {
					if <RemovedDats>::get().contains(&dat) {
						Self::force_clear_challenge(temporary_root.into(), user, challenge_index);
					}
					//todo charge fee*
				}
			}
			<RemovedDats>::kill();
		}

		//end Module
	}
}


/// TODO: tests for this module
/// TODO: get some reference test vectors for the proof
#[cfg(test)]
mod tests {
	use super::*;

	use runtime_io::with_externalities;
	use primitives::{H256, Blake2Hasher};
	use support::{impl_outer_origin, assert_ok, parameter_types};
	use sr_primitives::{traits::{BlakeTwo256, IdentityLookup}, testing::Header};
	use sr_primitives::weights::Weight;
	use sr_primitives::Perbill;

	impl_outer_origin! {
		pub enum Origin for Test {}
	}

	// For testing the module, we construct most of a mock runtime. This means
	// first constructing a configuration type (`Test`) which `impl`s each of the
	// configuration traits of modules we want to use.
	#[derive(Clone, Eq, PartialEq)]
	pub struct Test;
	parameter_types! {
		pub const BlockHashCount: u64 = 250;
		pub const MaximumBlockWeight: Weight = 1024;
		pub const MaximumBlockLength: u32 = 2 * 1024;
		pub const AvailableBlockRatio: Perbill = Perbill::from_percent(75);
	}
	impl system::Trait for Test {
		type Origin = Origin;
		type Call = ();
		type Index = u64;
		type BlockNumber = u64;
		type Hash = H256;
		type Hashing = BlakeTwo256;
		type AccountId = u64;
		type Lookup = IdentityLookup<Self::AccountId>;
		type Header = Header;
		type WeightMultiplierUpdate = ();
		type Event = ();
		type BlockHashCount = BlockHashCount;
		type MaximumBlockWeight = MaximumBlockWeight;
		type MaximumBlockLength = MaximumBlockLength;
		type AvailableBlockRatio = AvailableBlockRatio;
		type Version = ();
	}
	impl Trait for Test {
		type Event = ();
	}
	type TemplateModule = Module<Test>;

	// This function basically just builds a genesis storage key/value store according to
	// our desired mockup.
	fn new_test_ext() -> runtime_io::TestExternalities<Blake2Hasher> {
		system::GenesisConfig::default().build_storage::<Test>().unwrap().into()
	}

	#[test]
	fn it_works_for_default_value() {
		with_externalities(&mut new_test_ext(), || {
			// Just a dummy test for the dummy funtion `do_something`
			// calling the `do_something` function with a value 42
			assert_ok!(TemplateModule::do_something(Origin::signed(1), 42));
			// asserting that the stored value is equal to what we stored
			assert_eq!(TemplateModule::something(), Some(42));
		});
	}
}

/// runtime module implementing Substrate side of Erc20SubstrateBridge token exchange bridge
/// You can use mint to create tokens backed by locked funds on Ethereum side
/// and transfer tokens on substrate side freely
///
use crate::token;
use crate::types::{
    BridgeMessage, BridgeTransfer, Kind, MemberId, ProposalId, Status, TokenBalance,
    TransferMessage, ValidatorMessage,
};
use parity_codec::Encode;
use primitives::H160;
use runtime_primitives::traits::{As, Hash};
use support::{
    decl_event, decl_module, decl_storage, dispatch::Result, ensure, StorageMap, StorageValue,
};
use system::{self, ensure_signed};

const MAX_VALIDATORS: u32 = 100_000;

decl_event!(
    pub enum Event<T>
    where
        AccountId = <T as system::Trait>::AccountId,
        Hash = <T as system::Trait>::Hash,
    {
        RelayMessage(Hash),
        ApprovedRelayMessage(Hash, AccountId, H160, TokenBalance),
        Minted(Hash),
        Burned(Hash, AccountId, H160, TokenBalance),
    }
);

pub trait Trait: token::Trait + system::Trait {
    type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
}

decl_storage! {
    trait Store for Module<T: Trait> as Bridge {
        BridgeIsOperational get(bridge_is_operational): bool = true;
        BridgeMessages get(bridge_messages): map (T::Hash) => BridgeMessage<T::AccountId, T::Hash>;

        BridgeTransfers get(transfers): map ProposalId => BridgeTransfer<T::Hash>;
        BridgeTransfersCount get(bridge_transfers_count): ProposalId;
        TransferMessages get(messages): map(T::Hash) => TransferMessage<T::AccountId, T::Hash>;
        TransferId get(transfer_id_by_hash): map(T::Hash) => ProposalId;
        MessageId get(message_id_by_transfer_id): map(ProposalId) => T::Hash;


        ValidatorsCount get(validators_count) config(): u32 = 3;
        ValidatorHistory get(validator_history): map (T::Hash) => ValidatorMessage<T::AccountId, T::Hash>;
        Validators get(validators) build(|config: &GenesisConfig<T>| {
            config.validator_accounts.clone().into_iter()
            .map(|acc: T::AccountId| (acc, true)).collect::<Vec<_>>()
        }): map (T::AccountId) => bool;
    }
    add_extra_genesis {
        config(validator_accounts): Vec<T::AccountId>;
    }
}

decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        fn deposit_event<T>() = default;

        // initiate substrate -> ethereum transfer.
        // create proposition and emit the RelayMessage event
        fn set_transfer(origin, to: H160, #[compact] amount: TokenBalance)-> Result
        {
            let from = ensure_signed(origin)?;
            ensure!(Self::bridge_is_operational(), "Bridge is not operational");

            let transfer_hash = (&from, &to, amount, T::BlockNumber::sa(0)).using_encoded(<T as system::Trait>::Hashing::hash);

            let message = TransferMessage{
                message_id: transfer_hash,
                eth_address: to,
                substrate_address: from,
                amount,
                status: Status::Withdraw,
                action: Status::Withdraw,
            };
            Self::get_transfer_id_checked(transfer_hash, Kind::Transfer)?;
            Self::deposit_event(RawEvent::RelayMessage(transfer_hash));

            <TransferMessages<T>>::insert(transfer_hash, message);
            Ok(())
        }

        // ethereum-side multi-signed mint operation
        fn multi_signed_mint(origin, message_id: T::Hash, from: H160, to: T::AccountId, #[compact] amount: TokenBalance)-> Result {
            let validator = ensure_signed(origin)?;
            ensure!(Self::bridge_is_operational(), "Bridge is not operational");

            Self::check_validator(validator)?;

            if !<TransferMessages<T>>::exists(message_id) {
                let message = TransferMessage{
                    message_id,
                    eth_address: from,
                    substrate_address: to,
                    amount,
                    status: Status::Deposit,
                    action: Status::Deposit,
                };
                <TransferMessages<T>>::insert(message_id, message);
                Self::get_transfer_id_checked(message_id, Kind::Transfer)?;
            }

            let transfer_id = <TransferId<T>>::get(message_id);
            Self::_sign(transfer_id)?;

            Ok(())
        }

        // validator`s response to RelayMessage
        fn approve_transfer(origin, message_id: T::Hash) -> Result {
            let validator = ensure_signed(origin)?;
            ensure!(Self::bridge_is_operational(), "Bridge is not operational");
            Self::check_validator(validator)?;

            let id = <TransferId<T>>::get(message_id);
            Self::_sign(id)
        }

        // each validator calls it to add new validator
        fn add_validator(origin, address: T::AccountId) -> Result {
            let validator = ensure_signed(origin)?;
            ensure!(Self::bridge_is_operational(), "Bridge is not operational");
            Self::check_validator(validator)?;

            ensure!(<ValidatorsCount<T>>::get() < 100_000, "Validators maximum reached.");
            let hash = ("add", &address).using_encoded(<T as system::Trait>::Hashing::hash);

            if !<ValidatorHistory<T>>::exists(hash) {
                let message = ValidatorMessage {
                    message_id: hash,
                    account: address,
                    action: Status::AddValidator,
                    status: Status::AddValidator,
                };
                <ValidatorHistory<T>>::insert(hash, message);
                Self::get_transfer_id_checked(hash, Kind::Validator)?;
            }

            let id = <TransferId<T>>::get(hash);
            Self::_sign(id)
        }
        // each validator calls it to remove new validator
        fn remove_validator(origin, address: T::AccountId) -> Result {
            let validator = ensure_signed(origin)?;
            ensure!(Self::bridge_is_operational(), "Bridge is not operational");
            Self::check_validator(validator)?;

            ensure!(<ValidatorsCount<T>>::get() > 1, "Can not remove last validator.");

            let hash = ("remove", &address).using_encoded(<T as system::Trait>::Hashing::hash);

            if !<ValidatorHistory<T>>::exists(hash) {
                let message = ValidatorMessage {
                    message_id: hash,
                    account: address,
                    action: Status::RemoveValidator,
                    status: Status::RemoveValidator,
                };
                <ValidatorHistory<T>>::insert(hash, message);
                Self::get_transfer_id_checked(hash, Kind::Validator)?;
            }

            let id = <TransferId<T>>::get(hash);
            Self::_sign(id)
        }

        // each validator calls it to pause the bridge
        fn pause_bridge(origin) -> Result {
            let validator = ensure_signed(origin)?;
            Self::check_validator(validator.clone())?;

            ensure!(Self::bridge_is_operational(), "Bridge is not operational already");
            let hash = ("pause", T::BlockNumber::sa(0)).using_encoded(<T as system::Trait>::Hashing::hash);

            if !<BridgeMessages<T>>::exists(hash) {
                let message = BridgeMessage {
                    message_id: hash,
                    account: validator,
                    action: Status::PauseTheBridge,
                    status: Status::PauseTheBridge,
                };
                <BridgeMessages<T>>::insert(hash, message);
                Self::get_transfer_id_checked(hash, Kind::Bridge)?;
            }

            let id = <TransferId<T>>::get(hash);
            Self::_sign(id)
        }

        // each validator calls it to resume the bridge
        fn resume_bridge(origin) -> Result {
            let validator = ensure_signed(origin)?;
            Self::check_validator(validator.clone())?;

            let hash = ("resume", T::BlockNumber::sa(0)).using_encoded(<T as system::Trait>::Hashing::hash);

            if !<BridgeMessages<T>>::exists(hash) {
                let message = BridgeMessage {
                    message_id: hash,
                    account: validator,
                    action: Status::ResumeTheBridge,
                    status: Status::ResumeTheBridge,
                };
                <BridgeMessages<T>>::insert(hash, message);
                Self::get_transfer_id_checked(hash, Kind::Bridge)?;
            }

            let id = <TransferId<T>>::get(hash);
            Self::_sign(id)
        }

        //confirm burn from validator
        fn confirm_transfer(origin, message_id: T::Hash) -> Result {
            let validator = ensure_signed(origin)?;
            ensure!(Self::bridge_is_operational(), "Bridge is not operational");
            Self::check_validator(validator)?;

            let id = <TransferId<T>>::get(message_id);

            let is_approved = <TransferMessages<T>>::get(message_id).status == Status::Approved ||
            <TransferMessages<T>>::get(message_id).status == Status::Confirmed;
            ensure!(is_approved, "This transfer must be approved first.");

            Self::update_status(message_id, Status::Confirmed, Kind::Transfer)?;
            Self::reopen_for_burn_confirmation(message_id)?;
            Self::_sign(id)?;

            Ok(())
        }

        //cancel burn from validator
        fn cancel_transfer(origin, message_id: T::Hash) -> Result {
            let validator = ensure_signed(origin)?;
            ensure!(Self::bridge_is_operational(), "Bridge is not operational");
            Self::check_validator(validator)?;

            let mut message = <TransferMessages<T>>::get(message_id);
            message.status = Status::Canceled;

            <token::Module<T>>::unlock(&message.substrate_address, message.amount)?;
            <TransferMessages<T>>::insert(message_id, message);

            Ok(())
        }
    }
}

impl<T: Trait> Module<T> {
    fn _sign(transfer_id: ProposalId) -> Result {
        let mut transfer = <BridgeTransfers<T>>::get(transfer_id);

        let mut message = <TransferMessages<T>>::get(transfer.message_id);
        let mut validator_message = <ValidatorHistory<T>>::get(transfer.message_id);
        let mut bridge_message = <BridgeMessages<T>>::get(transfer.message_id);
        ensure!(transfer.open, "This transfer is not open");
        transfer.votes += 1;

        if Self::votes_are_enough(transfer.votes) {
            match message.status {
                Status::Confirmed => (), // if burn is confirmed
                _ => match transfer.kind {
                    Kind::Transfer => message.status = Status::Approved,
                    Kind::Validator => validator_message.status = Status::Approved,
                    Kind::Bridge => bridge_message.status = Status::Approved,
                },
            }
            match transfer.kind {
                Kind::Transfer => Self::execute_transfer(message)?,
                Kind::Validator => Self::manage_validator(validator_message)?,
                Kind::Bridge => Self::manage_bridge(bridge_message)?,
            }
            transfer.open = false;
        } else {
            match message.status {
                Status::Confirmed => (),
                _ => Self::update_status(
                    transfer.message_id,
                    Status::Pending,
                    transfer.clone().kind,
                )?,
            };
        }

        <BridgeTransfers<T>>::insert(transfer_id, transfer);

        Ok(())
    }

    ///ensure that such transfer exist
    fn get_transfer_id_checked(transfer_hash: T::Hash, kind: Kind) -> Result {
        if !<TransferId<T>>::exists(transfer_hash) {
            Self::create_transfer(transfer_hash, kind)?;
        }

        Ok(())
    }

    fn pause_the_bridge(message: BridgeMessage<T::AccountId, T::Hash>) -> Result {
        <BridgeIsOperational<T>>::mutate(|x| *x = false);
        Self::update_status(message.message_id, Status::Confirmed, Kind::Bridge)
    }

    fn resume_the_bridge(message: BridgeMessage<T::AccountId, T::Hash>) -> Result {
        <BridgeIsOperational<T>>::mutate(|x| *x = true);
        Self::update_status(message.message_id, Status::Confirmed, Kind::Bridge)
    }

    /// add validator
    fn _add_validator(info: ValidatorMessage<T::AccountId, T::Hash>) -> Result {
        ensure!(<ValidatorsCount<T>>::get() < MAX_VALIDATORS, "Validators maximum reached.");
        <Validators<T>>::insert(info.account, true);
        <ValidatorsCount<T>>::mutate(|x| *x += 1);
        Self::update_status(info.message_id, Status::Confirmed, Kind::Validator)
    }

    /// remove validator
    fn _remove_validator(info: ValidatorMessage<T::AccountId, T::Hash>) -> Result {
        ensure!(<ValidatorsCount<T>>::get() > 1, "Can not remove last validator.");
        <Validators<T>>::remove(info.account);
        <ValidatorsCount<T>>::mutate(|x| *x -= 1);
        <ValidatorHistory<T>>::remove(info.message_id);
        Ok(())
    }

    /// check votes validity
    fn votes_are_enough(votes: MemberId) -> bool {
        votes as f64 / Self::validators_count() as f64 >= 0.51
    }

    /// lock funds after set_transfer call
    fn lock_for_burn(account: T::AccountId, amount: TokenBalance) -> Result {
        <token::Module<T>>::lock(account, amount)?;

        Ok(())
    }

    fn execute_burn(message_id: T::Hash) -> Result {
        let message = <TransferMessages<T>>::get(message_id);
        let from = message.substrate_address.clone();
        let to = message.eth_address;

        <token::Module<T>>::unlock(&from, message.amount)?;
        <token::Module<T>>::_burn(from.clone(), message.amount)?;

        Self::deposit_event(RawEvent::Burned(message_id, from, to, message.amount));
        Ok(())
    }

    fn execute_transfer(message: TransferMessage<T::AccountId, T::Hash>) -> Result {
        match message.action {
            Status::Deposit => match message.status {
                Status::Approved => {
                    let to = message.substrate_address.clone();
                    <token::Module<T>>::_mint(to, message.amount)?;
                    Self::deposit_event(RawEvent::Minted(message.message_id));
                    Self::update_status(message.message_id, Status::Confirmed, Kind::Transfer)
                }
                _ => Err("Tried to deposit with non-supported status"),
            },
            Status::Withdraw => match message.status {
                Status::Confirmed => Self::execute_burn(message.message_id),
                Status::Approved => {
                    let to = message.eth_address;
                    let from = message.substrate_address.clone();
                    Self::lock_for_burn(from.clone(), message.amount)?;
                    Self::deposit_event(RawEvent::ApprovedRelayMessage(
                        message.message_id,
                        from,
                        to,
                        message.amount,
                    ));
                    Self::update_status(message.message_id, Status::Approved, Kind::Transfer)
                }
                _ => Err("Tried to withdraw with non-supported status"),
            },
            _ => Err("Tried to execute transfer with non-supported status"),
        }
    }

    fn manage_validator(message: ValidatorMessage<T::AccountId, T::Hash>) -> Result {
        match message.action {
            Status::AddValidator => match message.status {
                Status::Approved => Self::_add_validator(message),
                _ => Err("Tried to add validator with non-supported status"),
            },
            Status::RemoveValidator => match message.status {
                Status::Approved => Self::_remove_validator(message),
                _ => Err("Tried to remove validator with non-supported status"),
            },
            _ => Err("Tried to manage validator with non-supported status"),
        }
    }

    fn manage_bridge(message: BridgeMessage<T::AccountId, T::Hash>) -> Result {
        match message.action {
            Status::PauseTheBridge => match message.status {
                Status::Approved => Self::pause_the_bridge(message),
                _ => Err("Tried to pause the bridge with non-supported status"),
            },
            Status::ResumeTheBridge => match message.status {
                Status::Approved => Self::resume_the_bridge(message),
                _ => Err("Tried to resume the bridge with non-supported status"),
            },
            _ => Err("Tried to manage bridge with non-supported status"),
        }
    }

    fn create_transfer(transfer_hash: T::Hash, kind: Kind) -> Result {
        ensure!(
            !<TransferId<T>>::exists(transfer_hash),
            "This transfer already open"
        );

        let transfer_id = <BridgeTransfersCount<T>>::get();
        let bridge_transfers_count = <BridgeTransfersCount<T>>::get();
        let new_bridge_transfers_count = bridge_transfers_count
            .checked_add(1)
            .ok_or("Overflow adding a new bridge transfer")?;

        let transfer = BridgeTransfer {
            transfer_id,
            message_id: transfer_hash,
            open: true,
            votes: 0,
            kind,
        };

        <BridgeTransfers<T>>::insert(transfer_id, transfer);
        <BridgeTransfersCount<T>>::mutate(|count| *count = new_bridge_transfers_count);
        <TransferId<T>>::insert(transfer_hash, transfer_id);
        <MessageId<T>>::insert(transfer_id, transfer_hash);

        Ok(())
    }

    fn update_status(id: T::Hash, status: Status, kind: Kind) -> Result {
        match kind {
            Kind::Transfer => {
                let mut message = <TransferMessages<T>>::get(id);
                message.status = status;
                <TransferMessages<T>>::insert(id, message);
            }
            Kind::Validator => {
                let mut message = <ValidatorHistory<T>>::get(id);
                message.status = status;
                <ValidatorHistory<T>>::insert(id, message);
            }
            Kind::Bridge => {
                let mut message = <BridgeMessages<T>>::get(id);
                message.status = status;
                <BridgeMessages<T>>::insert(id, message);
            }
        }
        Ok(())
    }
    fn reopen_for_burn_confirmation(message_id: T::Hash) -> Result {
        let message = <TransferMessages<T>>::get(message_id);
        let transfer_id = <TransferId<T>>::get(message_id);
        let mut transfer = <BridgeTransfers<T>>::get(transfer_id);
        if !transfer.open && message.status == Status::Confirmed {
            transfer.votes = 0;
            transfer.open = true;
            <BridgeTransfers<T>>::insert(transfer_id, transfer);
        }
        Ok(())
    }
    fn check_validator(validator: T::AccountId) -> Result {
        let is_trusted = <Validators<T>>::exists(validator);
        ensure!(is_trusted, "Only validators can call this function");

        Ok(())
    }
}

/// tests for this module
#[cfg(test)]
mod tests {
    use super::*;

    use primitives::{Blake2Hasher, H160, H256};
    use runtime_io::with_externalities;
    use runtime_primitives::{
        testing::{Digest, DigestItem, Header},
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };
    use support::{assert_noop, assert_ok, impl_outer_origin};

    impl_outer_origin! {
        pub enum Origin for Test {}
    }

    // For testing the module, we construct most of a mock runtime. This means
    // first constructing a configuration type (`Test`) which `impl`s each of the
    // configuration traits of modules we want to use.
    #[derive(Clone, Eq, PartialEq)]
    pub struct Test;
    impl system::Trait for Test {
        type Origin = Origin;
        type Index = u64;
        type BlockNumber = u64;
        type Hash = H256;
        type Hashing = BlakeTwo256;
        type Digest = Digest;
        type AccountId = u64;
        type Lookup = IdentityLookup<Self::AccountId>;
        type Header = Header;
        type Event = ();
        type Log = DigestItem;
    }
    impl balances::Trait for Test {
        type Balance = u128;
        type OnFreeBalanceZero = ();
        type OnNewAccount = ();
        type TransactionPayment = ();
        type TransferPayment = ();
        type DustRemoval = ();
        type Event = ();
    }
    impl timestamp::Trait for Test {
        type Moment = u64;
        type OnTimestampSet = ();
    }
    impl token::Trait for Test {
        type Event = ();
    }
    impl Trait for Test {
        type Event = ();
    }

    type BridgeModule = Module<Test>;
    type TokenModule = token::Module<Test>;

    const ETH_MESSAGE_ID: &[u8; 32] = b"0x5617efe391571b5dc8230db92ba65b";
    const ETH_ADDRESS: &[u8; 20] = b"0x00b46c2526ebb8f4c9";
    const V1: u64 = 1;
    const V2: u64 = 2;
    const V3: u64 = 3;
    const V4: u64 = 4;
    const USER1: u64 = 4;
    const USER2: u64 = 5;

    // This function basically just builds a genesis storage key/value store according to
    // our desired mockup.
    fn new_test_ext() -> runtime_io::TestExternalities<Blake2Hasher> {
        let mut r = system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap()
            .0;

        r.extend(
            balances::GenesisConfig::<Test> {
                balances: vec![
                    (V1, 100000),
                    (V2, 100000),
                    (V3, 100000),
                    (USER1, 100000),
                    (USER2, 300000),
                ],
                vesting: vec![],
                transaction_base_fee: 0,
                transaction_byte_fee: 0,
                existential_deposit: 500,
                transfer_fee: 0,
                creation_fee: 0,
            }
            .build_storage()
            .unwrap()
            .0,
        );

        r.extend(
            GenesisConfig::<Test> {
                validators_count: 3u32,
                validator_accounts: vec![V1, V2, V3],
            }
            .build_storage()
            .unwrap()
            .0,
        );

        r.into()
    }

    #[test]
    fn token_eth2sub_mint_works() {
        with_externalities(&mut new_test_ext(), || {
            let message_id = H256::from(ETH_MESSAGE_ID);
            let eth_address = H160::from(ETH_ADDRESS);

            //substrate <----- ETH
            assert_ok!(BridgeModule::multi_signed_mint(
                Origin::signed(V2),
                message_id,
                eth_address,
                USER2,
                1000
            ));
            let mut message = BridgeModule::messages(message_id);
            assert_eq!(message.status, Status::Pending);

            assert_ok!(BridgeModule::multi_signed_mint(
                Origin::signed(V1),
                message_id,
                eth_address,
                USER2,
                1000
            ));
            message = BridgeModule::messages(message_id);
            assert_eq!(message.status, Status::Confirmed);

            let transfer = BridgeModule::transfers(0);
            assert_eq!(transfer.open, false);

            assert_eq!(TokenModule::balance_of(USER2), 1000);
            assert_eq!(TokenModule::total_supply(), 1000);
        })
    }
    #[test]
    fn token_eth2sub_closed_transfer_fail() {
        with_externalities(&mut new_test_ext(), || {
            let message_id = H256::from(ETH_MESSAGE_ID);
            let eth_address = H160::from(ETH_ADDRESS);

            //substrate <----- ETH
            assert_ok!(BridgeModule::multi_signed_mint(
                Origin::signed(V2),
                message_id,
                eth_address,
                USER2,
                1000
            ));
            assert_ok!(BridgeModule::multi_signed_mint(
                Origin::signed(V1),
                message_id,
                eth_address,
                USER2,
                1000
            ));
            assert_noop!(
                BridgeModule::multi_signed_mint(
                    Origin::signed(V3),
                    message_id,
                    eth_address,
                    USER2,
                    1000
                ),
                "This transfer is not open"
            );
            assert_eq!(TokenModule::balance_of(USER2), 1000);
            assert_eq!(TokenModule::total_supply(), 1000);
            let transfer = BridgeModule::transfers(0);
            assert_eq!(transfer.open, false);

            let message = BridgeModule::messages(message_id);
            assert_eq!(message.status, Status::Confirmed);
        })
    }

    #[test]
    fn token_sub2eth_burn_works() {
        with_externalities(&mut new_test_ext(), || {
            let eth_message_id = H256::from(ETH_MESSAGE_ID);
            let eth_address = H160::from(ETH_ADDRESS);

            //substrate <----- ETH
            assert_ok!(BridgeModule::multi_signed_mint(
                Origin::signed(V2),
                eth_message_id,
                eth_address,
                USER2,
                1000
            ));
            assert_ok!(BridgeModule::multi_signed_mint(
                Origin::signed(V1),
                eth_message_id,
                eth_address,
                USER2,
                1000
            ));

            //substrate ----> ETH
            assert_ok!(BridgeModule::set_transfer(
                Origin::signed(USER2),
                eth_address,
                500
            ));
            //RelayMessage(message_id) event emitted

            let sub_message_id = BridgeModule::message_id_by_transfer_id(1);
            let get_message = || BridgeModule::messages(sub_message_id);

            let mut message = get_message();
            assert_eq!(message.status, Status::Withdraw);

            //approval
            assert_eq!(TokenModule::locked(USER2), 0);
            assert_ok!(BridgeModule::approve_transfer(
                Origin::signed(V1),
                sub_message_id
            ));
            assert_ok!(BridgeModule::approve_transfer(
                Origin::signed(V2),
                sub_message_id
            ));

            message = get_message();
            assert_eq!(message.status, Status::Approved);

            // at this point transfer is in Approved status and are waiting for confirmation
            // from ethereum side to burn. Funds are locked.
            assert_eq!(TokenModule::locked(USER2), 500);
            assert_eq!(TokenModule::balance_of(USER2), 1000);
            // once it happends, validators call confirm_transfer

            assert_ok!(BridgeModule::confirm_transfer(
                Origin::signed(V2),
                sub_message_id
            ));

            message = get_message();
            let transfer = BridgeModule::transfers(1);
            assert_eq!(message.status, Status::Confirmed);
            assert_eq!(transfer.open, true);
            assert_ok!(BridgeModule::confirm_transfer(
                Origin::signed(V1),
                sub_message_id
            ));
            // assert_ok!(BridgeModule::confirm_transfer(Origin::signed(USER1), sub_message_id));
            //Burned(Hash, AccountId, H160, u64) event emitted

            assert_eq!(TokenModule::balance_of(USER2), 500);
            assert_eq!(TokenModule::total_supply(), 500);
        })
    }
    #[test]
    fn token_sub2eth_burn_fail_skip_approval() {
        with_externalities(&mut new_test_ext(), || {
            let eth_message_id = H256::from(ETH_MESSAGE_ID);
            let eth_address = H160::from(ETH_ADDRESS);

            //substrate <----- ETH
            assert_ok!(BridgeModule::multi_signed_mint(
                Origin::signed(V2),
                eth_message_id,
                eth_address,
                USER2,
                1000
            ));
            assert_ok!(BridgeModule::multi_signed_mint(
                Origin::signed(V1),
                eth_message_id,
                eth_address,
                USER2,
                1000
            ));
            assert_eq!(TokenModule::balance_of(USER2), 1000);
            assert_eq!(TokenModule::total_supply(), 1000);

            //substrate ----> ETH
            assert_ok!(BridgeModule::set_transfer(
                Origin::signed(USER2),
                eth_address,
                500
            ));
            //RelayMessage(message_id) event emitted

            let sub_message_id = BridgeModule::message_id_by_transfer_id(1);
            let message = BridgeModule::messages(sub_message_id);
            assert_eq!(message.status, Status::Withdraw);

            assert_eq!(TokenModule::locked(USER2), 0);
            // lets say validators blacked out and we
            // try to confirm without approval anyway
            assert_noop!(
                BridgeModule::confirm_transfer(Origin::signed(V1), sub_message_id),
                "This transfer must be approved first."
            );
        })
    }
    #[test]
    fn add_validator_should_work() {
        with_externalities(&mut new_test_ext(), || {
            assert_ok!(BridgeModule::add_validator(Origin::signed(V2), V4));
            let id = BridgeModule::message_id_by_transfer_id(0);
            let mut message = BridgeModule::validator_history(id);
            assert_eq!(message.status, Status::Pending);

            assert_ok!(BridgeModule::add_validator(Origin::signed(V1), V4));
            message = BridgeModule::validator_history(id);
            assert_eq!(message.status, Status::Confirmed);
            assert_eq!(BridgeModule::validators_count(), 4);
        })
    }
    #[test]
    fn remove_validator_should_work() {
        with_externalities(&mut new_test_ext(), || {
            assert_ok!(BridgeModule::remove_validator(Origin::signed(V2), V3));
            let id = BridgeModule::message_id_by_transfer_id(0);
            let mut message = BridgeModule::validator_history(id);
            assert_eq!(message.status, Status::Pending);

            assert_ok!(BridgeModule::remove_validator(Origin::signed(V1), V3));
            message = BridgeModule::validator_history(id);
            assert_eq!(message.status, Status::Revoked);
            assert_eq!(BridgeModule::validators_count(), 2);
        })
    }
    #[test]
    fn remove_last_validator_should_fail() {
        with_externalities(&mut new_test_ext(), || {
            assert_ok!(BridgeModule::remove_validator(Origin::signed(V2), V3));
            assert_ok!(BridgeModule::remove_validator(Origin::signed(V1), V3));
            assert_eq!(BridgeModule::validators_count(), 2);

            //TODO: deal with two validators corner case
            assert_ok!(BridgeModule::remove_validator(Origin::signed(V1), V2));
            assert_ok!(BridgeModule::remove_validator(Origin::signed(V2), V2));
            // ^ this guy probably will not sign his removal ^

            assert_eq!(BridgeModule::validators_count(), 1);
            // TODO: fails through different hashes
            // assert_ok fails with corect error but the noop below fails with different hashes
            // assert_noop!(BridgeModule::remove_validator(Origin::signed(V1), V1), "Cant remove last validator");
        })
    }
    #[test]
    fn pause_the_bridge_should_work() {
        with_externalities(&mut new_test_ext(), || {
            assert_ok!(BridgeModule::pause_bridge(Origin::signed(V2)));

            assert_eq!(BridgeModule::bridge_transfers_count(), 1);
            assert_eq!(BridgeModule::bridge_is_operational(), true);
            let id = BridgeModule::message_id_by_transfer_id(0);
            let mut message = BridgeModule::bridge_messages(id);
            assert_eq!(message.status, Status::Pending);

            assert_ok!(BridgeModule::pause_bridge(Origin::signed(V1)));
            assert_eq!(BridgeModule::bridge_is_operational(), false);
            message = BridgeModule::bridge_messages(id);
            assert_eq!(message.status, Status::Confirmed);
        })
    }
    #[test]
    fn extrinsics_restricted_should_fail() {
        with_externalities(&mut new_test_ext(), || {
            let eth_message_id = H256::from(ETH_MESSAGE_ID);
            let eth_address = H160::from(ETH_ADDRESS);

            assert_ok!(BridgeModule::pause_bridge(Origin::signed(V2)));
            assert_ok!(BridgeModule::pause_bridge(Origin::signed(V1)));

            // substrate <-- Ethereum
            assert_noop!(
                BridgeModule::multi_signed_mint(
                    Origin::signed(V2),
                    eth_message_id,
                    eth_address,
                    USER2,
                    1000
                ),
                "Bridge is not operational"
            );
        })
    }
    #[test]
    fn double_pause_should_fail() {
        with_externalities(&mut new_test_ext(), || {
            assert_eq!(BridgeModule::bridge_is_operational(), true);
            assert_ok!(BridgeModule::pause_bridge(Origin::signed(V2)));
            assert_ok!(BridgeModule::pause_bridge(Origin::signed(V1)));
            assert_eq!(BridgeModule::bridge_is_operational(), false);
            assert_noop!(BridgeModule::pause_bridge(Origin::signed(V1)), "Bridge is not operational already");
        })
    }
    #[test]
    fn pause_and_resume_the_bridge_should_work() {
        with_externalities(&mut new_test_ext(), || {
            assert_eq!(BridgeModule::bridge_is_operational(), true);
            assert_ok!(BridgeModule::pause_bridge(Origin::signed(V2)));
            assert_ok!(BridgeModule::pause_bridge(Origin::signed(V1)));
            assert_eq!(BridgeModule::bridge_is_operational(), false);
            assert_ok!(BridgeModule::resume_bridge(Origin::signed(V1)));
            assert_ok!(BridgeModule::resume_bridge(Origin::signed(V2)));
            assert_eq!(BridgeModule::bridge_is_operational(), true);
        })
    }
}

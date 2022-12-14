module account_recovery::hackathon {

    const ERECOVERY_NOT_SET: u64 = 1;
    const ERECOVERY_ALREADY_SET: u64 = 2;
    const EUNAUTHORIZED: u64 = 3;
    const ERECOVERY_NOT_IN_PROGRESS: u64 = 4;
    const ERECOVERY_ALREADY_IN_PROGRESS: u64 = 5;

    use std::signer;
    use aptos_std::table::{Table, add};
    use aptos_framework::account;
    use aptos_framework::timestamp;
    use aptos_std::table;
    use std::vector;
    use std::option::Option;
    use std::option;
    use aptos_framework::resource_account;
    use aptos_framework::account::SignerCapability;

    /// Account recovery struct
    struct AccountRecovery has key, store, drop {
        authorized_addresses: vector<address>,
        required_num_for_recovery: u64,
        required_delay_seconds: u64,
        rotate_valid_window_seconds: u64,
        allow_unauthorized_initiation: bool,
        account_recovery_init: Option<AccountRecoveryInitData>,
    }

    // Reverse lookup map
    struct AccountRecoveryReverseLookup has key, store {
        authorized_to_recovery: Table<address, vector<address>>,
    }

    struct AccountRecoveryInitData has key, store {
        initiator: address,
        authorized: vector<address>,
        recovery_seq_number: u64,
        recovery_initiation_ts: u64,
    }

    struct ModuleData has key {
        // Storing the signer capability here, so the module can programmatically sign for transactions
        signer_cap: SignerCapability,
    }

    fun init_module(signer: &signer) {
        move_to(signer, AccountRecoveryReverseLookup {
            authorized_to_recovery: table::new(),
        });

        let resource_signer_cap = resource_account::retrieve_resource_account_cap(signer, @source_addr);

        move_to(signer, ModuleData {
            signer_cap: resource_signer_cap,
        });
    }

    fun recovery_exists(addr: address): bool {
        exists<AccountRecovery>(addr)
    }

    public fun register_without_authorization(
        account: &signer,
        required_delay_seconds: u64,
        rotation_capability_sig_bytes: vector<u8>,
        account_public_key_bytes: vector<u8>,
    ) acquires AccountRecoveryReverseLookup {
        register(account, vector::empty(), 0, required_delay_seconds, required_delay_seconds, true, rotation_capability_sig_bytes, account_public_key_bytes);
    }

    public fun register_authorize_one(
        account: &signer,
        authorized_address: address,
        required_delay_seconds: u64,
        rotation_capability_sig_bytes: vector<u8>,
        account_public_key_bytes: vector<u8>,
    ) acquires AccountRecoveryReverseLookup {
        register(account, vector::singleton(authorized_address), 1, required_delay_seconds, required_delay_seconds, false, rotation_capability_sig_bytes, account_public_key_bytes);
    }

    public fun register(
        account: &signer,
        authorized_addresses: vector<address>,
        required_num_for_recovery: u64,
        required_delay_seconds: u64,
        rotate_valid_window_seconds: u64,
        allow_unauthorized_initiation: bool,
        rotation_capability_sig_bytes: vector<u8>,
        account_public_key_bytes: vector<u8>,
    ) acquires AccountRecoveryReverseLookup {
        let addr = signer::address_of(account);

        assert!(!recovery_exists(addr), ERECOVERY_ALREADY_SET);
        assert!(exists<AccountRecoveryReverseLookup>(@account_recovery));

        let reverse_lookup = borrow_global_mut<AccountRecoveryReverseLookup>(@account_recovery);

        move_to(account, AccountRecovery {
            authorized_addresses,
            required_num_for_recovery,
            required_delay_seconds,
            rotate_valid_window_seconds,
            allow_unauthorized_initiation,
            account_recovery_init: option::none<AccountRecoveryInitData>(),
        });

        let i = 0;
        let len = vector::length(&authorized_addresses);
        while (i < len) {
            let authorized_address = *vector::borrow(&authorized_addresses, i);
            let list = table::borrow_mut_with_default(&mut reverse_lookup.authorized_to_recovery, authorized_address, vector::empty<address>());
            vector::push_back(list, addr);
        };
        account::offer_rotation_capability(signer, rotation_capability_sig_bytes, 0, account_public_key_bytes, @account_recovery)
    }

    fun clear_stale_recovery(account_recovery: &mut AccountRecovery, recovery_address: address) {
        if (option::is_some(&account_recovery.account_recovery_init)) {
            let account_recovery_init = std::option::borrow(&account_recovery.account_recovery_init);

            let had_activity = account::get_sequence_number(recovery_address) != account_recovery_init.recovery_seq_number;
            let too_much_time_passed = timestamp::now_seconds() < account_recovery_init.recovery_initiation_ts + account_recovery.required_delay_seconds + account_recovery.rotate_valid_window_seconds;

            if (had_activity || too_much_time_passed) {
                account_recovery.account_recovery_init = option::none();
            };
        }
    }

    public fun initiate_account_key_recovery(account: &signer, recovery_address: address) acquires AccountRecovery {
        assert!(recovery_exists(recovery_address), ERECOVERY_NOT_SET);

        let account_recovery = borrow_global_mut<AccountRecovery>(recovery_address);
        clear_stale_recovery(account_recovery, recovery_address);
        assert!(account_recovery.account_recovery_init, ERECOVERY_ALREADY_IN_PROGRESS);

        let addr = signer::address_of(account);

        let initiator_authorized = vector::contains(&account_recovery.authorized_addresses, &addr);
        if (!account_recovery.allow_unauthorized_initiation) {
            assert!(initiator_authorized, EUNAUTHORIZED);
        };

        let authorized = if (initiator_authorized) {
            vector::singleton(addr)
        } else {
            vector::empty<address>()
        };

        account_recovery.account_recovery_init = std::option::some(AccountRecoveryInitData{
            initiator: addr,
            authorized,
            recovery_seq_number: account::get_sequence_number(recovery_address),
            recovery_initiation_ts: timestamp::now_seconds(),
        });
    }

    public fun authorize_key_recovery(account: &signer, recovery_address: address, initiator: address) acquires AccountRecovery {
        assert!(recovery_exists(recovery_address), ERECOVERY_NOT_SET);
        let account_recovery = borrow_global_mut<AccountRecovery>(recovery_address);
        clear_stale_recovery(account_recovery, recovery_address);

        assert!(std::option::is_some(&account_recovery.account_recovery_init), ERECOVERY_NOT_IN_PROGRESS);
        let account_recovery_init = std::option::borrow(&account_recovery.account_recovery_init);

        assert!(account_recovery_init.initiator == initiator);

        let addr = signer::address_of(account);
        assert!(!vector::contains(&account_recovery_init.authorized, &addr));

        vector::push_back(&mut account_recovery_init.authorized, addr);
    }

    public fun rotate_key(account: &signer,
                          recovery_address: address,
                          new_public_key_bytes: vector<u8>,
                          cap_update_table: vector<u8>) acquires AccountRecovery, ModuleData {
        assert!(recovery_exists(recovery_address), ERECOVERY_NOT_SET);

        let account_recovery = borrow_global_mut<AccountRecovery>(recovery_address);
        clear_stale_recovery(account_recovery, recovery_address);

        assert!(std::option::is_some(&account_recovery.account_recovery_init), ERECOVERY_NOT_IN_PROGRESS);
        let account_recovery_init = std::option::borrow(&account_recovery.account_recovery_init);

        let addr = signer::address_of(account);
        assert!(account_recovery_init.initiator == addr);
        assert!(vector::length(&account_recovery_init.authorized) >= account_recovery.required_num_for_recovery);

        assert!(timestamp::now_seconds() > account_recovery_init.recovery_initiation_ts + account_recovery.required_delay_seconds);

        let module_data = borrow_global_mut<ModuleData>(@account_recovery);
        let resource_signer = account::create_signer_with_capability(&module_data.signer_cap);

        account::rotate_authentication_key_with_rotation_capability(&resource_signer, recovery_address, 0, new_public_key_bytes, cap_update_table);

        account_recovery.account_recovery_init = option::none();
    }

    public fun deregister(account: &signer) acquires AccountRecovery, AccountRecoveryReverseLookup {
        let addr = signer::address_of(account);
        let previous = move_from<AccountRecovery>(addr);

        assert!(!exists<AccountRecoveryReverseLookup>(@account_recovery));
        account::revoke_rotation_capability(account, @account_recovery);

        let reverse_lookup = borrow_global_mut<AccountRecoveryReverseLookup>(@account_recovery);

        let i = 0;
        let len = vector::length(&previous.authorized_addresses);
        while (i < len) {
            let authorized_address = *vector::borrow(&previous.authorized_addresses, i);
            let list = table::borrow_mut_with_default(&mut reverse_lookup.authorized_to_recovery, authorized_address, vector::empty<address>());
            let (found, index) = vector::index_of(list, &addr);
            if (found) {
                vector::swap_remove(list, index);
            }
        };
    }

    #[test_only]
    public fun dummy() {}

    #[test(account = @0x123, authorized = @0x234)]
    #[expected_failure(abort_code = 0x10001, location = Self)]
    public entry fun test_recovery_not_set(
        account: &signer,
        authorized: &signer,
    ) {
        initiate_account_key_recovery(authorized, )
    }

}

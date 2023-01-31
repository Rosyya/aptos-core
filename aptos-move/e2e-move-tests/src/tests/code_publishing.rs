// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{assert_abort, assert_success, assert_vm_status, tests::common, MoveHarness};
use aptos_framework::natives::code::{PackageRegistry, UpgradePolicy};
use aptos_package_builder::PackageBuilder;
use aptos_types::{
    account_address::{create_resource_address, AccountAddress},
    on_chain_config::FeatureFlag,
};
use move_core_types::{parser::parse_struct_tag, vm_status::StatusCode};
use rstest::rstest;
use serde::{Deserialize, Serialize};
use aptos_cached_packages::aptos_stdlib;
use aptos_framework::{BuildOptions, BuiltPackage};

// Note: this module uses parameterized tests via the
// [`rstest` crate](https://crates.io/crates/rstest)
// to test for multiple feature combinations.

/// Mimics `0xcafe::test::State`
#[derive(Serialize, Deserialize)]
struct State {
    value: u64,
}

/// Mimics `0xcafe::test::State`
#[derive(Serialize, Deserialize)]
struct StateWithCoins {
    important_value: u64,
    value: u64,
}

/// Runs the basic publishing test for all legacy flag combinations. Otherwise we will only
/// run tests which are expected to make a difference for legacy flag combinations.
#[rstest(enabled, disabled,
    case(vec![], vec![FeatureFlag::CODE_DEPENDENCY_CHECK]),
    case(vec![FeatureFlag::CODE_DEPENDENCY_CHECK], vec![]),
)]
fn code_publishing_basic(enabled: Vec<FeatureFlag>, disabled: Vec<FeatureFlag>) {
    let mut h = MoveHarness::new_with_features(enabled, disabled);
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());
    assert_success!(h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_initial"),
    ));

    // Validate metadata as expected.
    let registry = h
        .read_resource::<PackageRegistry>(
            acc.address(),
            parse_struct_tag("0x1::code::PackageRegistry").unwrap(),
        )
        .unwrap();
    assert_eq!(registry.packages.len(), 1);
    assert_eq!(registry.packages[0].name, "test_package");
    assert_eq!(registry.packages[0].modules.len(), 1);
    assert_eq!(registry.packages[0].modules[0].name, "test");

    // Validate code loaded as expected.
    assert_success!(h.run_entry_function(
        &acc,
        str::parse("0xcafe::test::hello").unwrap(),
        vec![],
        vec![bcs::to_bytes::<u64>(&42).unwrap()]
    ));
    let state = h
        .read_resource::<State>(
            acc.address(),
            parse_struct_tag("0xcafe::test::State").unwrap(),
        )
        .unwrap();
    assert_eq!(state.value, 42)
}

#[test]
fn code_publishing_upgrade_success_compat() {
    let mut h = MoveHarness::new();
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());

    // Install the initial version with compat requirements
    assert_success!(h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_initial"),
    ));

    // We should be able to upgrade it with the compatible version
    assert_success!(h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_upgrade_compat"),
    ));
}

#[test]
fn code_publishing_upgrade_fail_compat() {
    let mut h = MoveHarness::new();
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());

    // Install the initial version with compat requirements
    assert_success!(h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_initial"),
    ));

    // We should not be able to upgrade it with the incompatible version
    let status = h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_upgrade_incompat"),
    );
    assert_vm_status!(status, StatusCode::BACKWARD_INCOMPATIBLE_MODULE_UPDATE)
}

#[test]
fn code_publishing_upgrade_fail_immutable() {
    let mut h = MoveHarness::new();
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());

    // Install the initial version with immutable requirements
    assert_success!(h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_initial_immutable"),
    ));

    // We should not be able to upgrade it with the compatible version
    let status = h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_upgrade_compat"),
    );
    assert_abort!(status, _);
}

#[test]
fn code_publishing_upgrade_fail_overlapping_module() {
    let mut h = MoveHarness::new();
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());

    // Install the initial version
    assert_success!(h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_initial"),
    ));

    // Install a different package with the same module.
    let status = h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_other_name"),
    );
    assert_abort!(status, _);
}

/// This test verifies that the cache incoherence bug on module upgrade is fixed. This bug
/// exposes itself by that after module upgrade the old version of the module stays
/// active until the MoveVM terminates. In order to workaround this until there is a better
/// fix, we flush the cache in `MoveVmExt::new_session`. One can verify the fix by commenting
/// the flush operation out, then this test fails.
///
/// TODO: for some reason this test did not capture a serious bug in `code::check_coexistence`.
#[test]
fn code_publishing_upgrade_loader_cache_consistency() {
    let mut h = MoveHarness::new();
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());

    // Create a sequence of package upgrades
    let txns = vec![
        h.create_publish_package(
            &acc,
            &common::test_dir_path("code_publishing.data/pack_initial"),
            None,
            |_| {},
        ),
        // Compatible with above package
        h.create_publish_package(
            &acc,
            &common::test_dir_path("code_publishing.data/pack_upgrade_compat"),
            None,
            |_| {},
        ),
        // Not compatible with above package, but with first one.
        // Correct behavior: should create backward_incompatible error
        // Bug behavior: succeeds because is compared with the first module
        h.create_publish_package(
            &acc,
            &common::test_dir_path("code_publishing.data/pack_compat_first_not_second"),
            None,
            |_| {},
        ),
    ];
    let result = h.run_block(txns);
    assert_success!(result[0]);
    assert_success!(result[1]);
    assert_vm_status!(result[2], StatusCode::BACKWARD_INCOMPATIBLE_MODULE_UPDATE)
}

#[test]
fn code_publishing_framework_upgrade() {
    let mut h = MoveHarness::new();
    let acc = h.aptos_framework_account();

    // We should be able to upgrade move-stdlib, as our local package has only
    // compatible changes. (We added a new function to string.move.)
    assert_success!(h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_stdlib"),
    ));
}

#[test]
fn code_publishing_framework_upgrade_fail() {
    let mut h = MoveHarness::new();
    let acc = h.aptos_framework_account();

    // We should not be able to upgrade move-stdlib because we removed a function
    // from the string module.
    let result = h.publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_stdlib_incompat"),
    );
    assert_vm_status!(result, StatusCode::BACKWARD_INCOMPATIBLE_MODULE_UPDATE)
}

#[test]
fn code_publishing_using_resource_account() {
    let mut h = MoveHarness::new();
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());

    let mut pack = PackageBuilder::new("Package1").with_policy(UpgradePolicy::compat());
    let module_address = create_resource_address(*acc.address(), &[]);
    pack.add_source(
        "m",
        &format!("module 0x{}::m {{ public fun f() {{}} }}", module_address),
    );
    let pack_dir = pack.write_to_temp().unwrap();
    let package = aptos_framework::BuiltPackage::build(
        pack_dir.path().to_owned(),
        aptos_framework::BuildOptions::default(),
    )
    .expect("building package must succeed");

    let code = package.extract_code();
    let metadata = package
        .extract_metadata()
        .expect("extracting package metadata must succeed");
    let bcs_metadata = bcs::to_bytes(&metadata).expect("PackageMetadata has BCS");

    let result = h.run_transaction_payload(
        &acc,
        aptos_cached_packages::aptos_stdlib::resource_account_create_resource_account_and_publish_package(
            vec![],
            bcs_metadata,
            code,
        ),
    );
    assert_success!(result);
}

#[test]
fn code_publishing_with_two_attempts_and_verify_loader_is_invalidated() {
    let mut h = MoveHarness::new();
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());

    // First module publish attempt failed when executing the init_module.
    // Second attempt should pass.
    // We expect the correct logic in init_module to be executed from the second attempt so the
    // value stored is from the second code, and not the first (which would be the case if the
    // VM's loader cache is not properly cleared after the first attempt).
    //
    // Depending on how the loader cache is flushed, the second attempt might even fail if the
    // entire init_module from the first attempt still lingers around and will fail if invoked.
    let failed_module_publish = h.create_publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_init_module_failed"),
        None,
        |_| {},
    );
    let module_publish_second_attempt = h.create_publish_package(
        &acc,
        &common::test_dir_path("code_publishing.data/pack_init_module_second_attempt"),
        None,
        |_| {},
    );
    let results = h.run_block(vec![failed_module_publish, module_publish_second_attempt]);
    assert_abort!(results[0], _);
    assert_success!(results[1]);

    let value_resource = h
        .read_resource::<StateWithCoins>(
            acc.address(),
            parse_struct_tag("0xcafe::test::State").unwrap(),
        )
        .unwrap();
    assert_eq!(2, value_resource.important_value);
}

#[rstest(enabled, disabled,
         case(vec![], vec![FeatureFlag::CODE_DEPENDENCY_CHECK]),
         case(vec![FeatureFlag::CODE_DEPENDENCY_CHECK], vec![]),
)]
fn code_publishing_faked_dependency(enabled: Vec<FeatureFlag>, disabled: Vec<FeatureFlag>) {
    let mut h = MoveHarness::new_with_features(enabled.clone(), disabled);
    let acc1 = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());
    let acc2 = h.new_account_at(AccountAddress::from_hex_literal("0xdeaf").unwrap());

    let mut pack1 = PackageBuilder::new("Package1").with_policy(UpgradePolicy::compat());
    pack1.add_source("m", "module 0xcafe::m { public fun f() {} }");
    let pack1_dir = pack1.write_to_temp().unwrap();
    assert_success!(h.publish_package(&acc1, pack1_dir.path()));

    // pack2 has a higher policy and should not be able to depend on pack1
    let mut pack2 = PackageBuilder::new("Package2").with_policy(UpgradePolicy::immutable());
    pack2.add_local_dep("Package1", &pack1_dir.path().to_string_lossy());
    pack2.add_source(
        "m",
        "module 0xdeaf::m { use 0xcafe::m; public fun f() { m::f() } }",
    );
    let pack2_dir = pack2.write_to_temp().unwrap();
    let result = h.publish_package_with_patcher(&acc2, pack2_dir.path(), |metadata| {
        // Hide the dependency from the lower policy package from the metadata. We detect this
        // this via checking the actual bytecode module dependencies.
        metadata.deps.clear()
    });
    if !enabled.contains(&FeatureFlag::CODE_DEPENDENCY_CHECK) {
        // In the previous version we were not able to detect this problem
        assert_success!(result)
    } else {
        assert_vm_status!(result, StatusCode::CONSTRAINT_NOT_SATISFIED)
    }
}

#[rstest(enabled, disabled,
         case(vec![], vec![FeatureFlag::TREAT_FRIEND_AS_PRIVATE]),
         case(vec![FeatureFlag::TREAT_FRIEND_AS_PRIVATE], vec![]),
)]
fn code_publishing_friend_as_private(enabled: Vec<FeatureFlag>, disabled: Vec<FeatureFlag>) {
    let mut h = MoveHarness::new_with_features(enabled.clone(), disabled);
    let acc = h.new_account_at(AccountAddress::from_hex_literal("0xcafe").unwrap());

    let mut pack1 = PackageBuilder::new("Package").with_policy(UpgradePolicy::compat());
    pack1.add_source(
        "m",
        "module 0xcafe::m { public fun f() {}  public(friend) fun g() {} }",
    );
    let pack1_dir = pack1.write_to_temp().unwrap();
    assert_success!(h.publish_package(&acc, pack1_dir.path()));

    let mut pack2 = PackageBuilder::new("Package").with_policy(UpgradePolicy::compat());
    // Removes friend
    pack2.add_source("m", "module 0xcafe::m { public fun f() {} }");
    let pack2_dir = pack2.write_to_temp().unwrap();

    let result = h.publish_package(&acc, pack2_dir.path());
    if enabled.contains(&FeatureFlag::TREAT_FRIEND_AS_PRIVATE) {
        // With this feature we can remove friends
        assert_success!(result)
    } else {
        assert_vm_status!(result, StatusCode::BACKWARD_INCOMPATIBLE_MODULE_UPDATE)
    }
}

#[rstest(enabled, disabled,
case(vec![FeatureFlag::VM_BINARY_FORMAT_V6], vec![]),
)]
fn code_publishing_v5_binary_with_metadata(enabled: Vec<FeatureFlag>, disabled: Vec<FeatureFlag>) {
    let mut h = MoveHarness::new_with_features(enabled.clone(), disabled);
    let account = h.new_account_at(AccountAddress::from_hex_literal("0xb7d68860645e7aff66895b2367686f75871601275245949da71a70befe6ffb1d").unwrap());

    let publish_tx = h.create_transaction_payload(
        &account,
        aptos_stdlib::code_publish_package_txn(
            hex::decode("1142776172654170746f735374616b696e6701000000000000000040443143393932383232443338423139453941384444333641363544384633343237343034463745353142314343333136353739343935313431333031374545349b021f8b08000000000002ff6550416ec32010bcf38ac8979c6a4362c0aed4435aa91fe831b222308b63b9060bb0d3e717623787e6c6cece0c337b9e443b880e1a64c408bbb75df67e130e4e53b0fe2b88a1375d861670beb7266d498e739c2174164a39f01e7c837c5069857f488644125e9e11eda2fdcdbae11f1eec0066c58e196aad834bb4b5b36bc1aff08952fc41aa98624c2b700b6ce629874c69bf85dcc8922b56550cb39202175a3356d5541e8e8cb38a694e2b4e1826074e0f25adcb5a094e04c7123430ad2551a99982098c02d3f6e0f3fb213effc237a8eb43fc697f0d61f2af4511c7eb2cf3d68ec5bdcf4b8ab23d53993c12f6c8c19244017c3010673f4bd5bb04adccd12e503c2eb4c91ff3fe17ce93313da40100000210706f6f6c5f7536345f756e626f756e6480141f8b08000000000002ffcd5a59731b37127ed7afc0be24a44291a22cc9596ae52aad2d975d258b2e8b5e6f9e86e00c284e650ed61ca26847ff7dbb71cc00180c0fc5dec40fa608a04f747fdd0038383c3c2087e42e8c971123711a94f0314f335264d4ff3d4cee094d02e2d3c82f235ae0f77c41339693744e2859a669847ff96998e47d325930350dff91559a150be0097fd29c143059a4058dc46a1226281847399730f1334673066c6ed94ab059a451c032909e9059b9169c94f88c642c602c460e6146d863986beaf58137b2bf7ea4dcb07994ae463830ec938f282e2f685690b42c72c2e265b1460272d227779adc2b948a8a925508860c8f8f8f95a557301445a082cfc20726a6a466b05c19256525e98a2cc0037c95ee02702dd7a91eafb5272f24355d2e414c480bc1fb814625d893a531485fd12ce06c2a1927ba923fe78666b827b84eec8bb612c59d9ab6ff9baf6cb5ff0eb68b91199be396a0b9b83f61d2238cfa0b6b3348982b9182bc07dcd17d285679f0cc70207bf41734b91791a8cb35fda911a10f5e98169d3dcb81e7269373dc6b1169b9a6649f5ca3a1967d9c7620f4ba448f5e29fbcc8de16aa1b0d36a90eb8096a2506953c4e6052a31389079390375594467f96884e1e595e7a75e99ccd21268bf1d10f857e6906bcb22cdbdbc0846a382ce22e6e1ee79114bee8bc568f4ed8e45739e8f38d72313fcf8022b6ef8029ce1434f171543ce8a65599a5d1cf0c1c16060044b9216047c9cb3a4400ff3d0e70bfd34c90b727df7eeead3f5bbf1cd9beb4fdeed78e2bd1d7fbe7d3322a03eb8697851f104fc40acc0904a5312d364ade380995a3affc978ec7db8bafdcdd304dd29fe2735ffd734415503961759ba06b593239efc4d8e1fc7e31beffd1dd7f6fac3c7c96f8adb8b063709433a381510bc5c531dc570bfdb0c787f7bf7f9eddbf7afdf5fdf4e841195faa7174e97fb42f8823e30219acb04927e4c1f6b1871ef8190e08dff73fde9edcdf88b1275568bc20805f4d0c14a4a84d4642c50929a4e9b8c275737deeb3198d41070de2240ba6d2f092d36bc94212a083e5cfdd7fb7c7e5a85daafa7a7e72f4f4f8f5fbe7879fccfb3b3e1f9f04cae878828fda2ceccbc40a78aa4c27f5c518f7b8273eb5933c2046b4a0df284fa170d02f806e8076b5ed58bc0196fd89c965101acb03e4d16612e6a1ec3f40b70380e93300ebf329261b223b4f27c04e059b0048c8d97a55e9c79cd16b0120341d1d7a5bd4b57ec81653d640e1c122645d0df21604b89e73a3509d2e467289640847554c88c41e1701985403c5b8b60872e017598531f9c578b94e39e18d75cf454a3c96ba8fd05a43e68b3d2d26359cea2d027f3126ce42b3add91d8a36f2dfea3961ad89d0c4987cde7cc2f0088234c7bb5a65b2b29d80ba83415ee0cbb5bb51505d22f216a623235e9a72da63865397cd5b4d8fada08cfe39e634ec5a235a9867935188dc020334e3b5d6bbda1603df764fbe88d045988630d634570f3de694e43e8fae67507886987903fa311c5eea2ea2b6dff49fcf638df0ed20a177535a7d01c82bbf8079fed6bde219797e002913da3519840371006502c31b69aa8dfed5e541c23566cf5bcb7c9f39ecbf39bbc6b903c017ea131b54272cb0c6f589b27644823b4cdf9c48a324bc814194e2b18deea78cd56e9f69fb8df05bed67eb19dde22be3e1124653c633c571580f9599ac34714353a00a1748b6e827c57e5c4ea36ed326813213ea79a0253ecf5da7580a25350cb393ddd801191fb035acdcc5892bb59b1f889eb298344e7d1dde0cd861f53d4bf0064aff46f55bee9ba56d54d87828f3a86e5a6ba5d2b610ea5a5b3147270b5cd4e11fa2cca99c5e6b81d7a3487a8900e1359d1d2c6864a87f4a7b64324ed333c8250015b21e30b1257f3ad69e385d52b7845ea89a22b17d76cf6d8f7edd9a22f831c55f2da72466e993843185bd6d0eaf332c0dab82fb4949ccc73204c5c0adced61b5f5ec3eacbb0176c0f116490308af226c68b046a9232bb40c8975ff806d4510c88b06c11986b49b0bf3bc5a9137ad8453b217264dcb5c51d513a26438a856c0957af52a5eddba7024e191712c7bdb2a24c11955480a028c372338756e5a74aa722a1b6a72d474f6ab4b83b8516369765fc67042ecb49f13babb4a9456bcbad46c7aaec04de1d318fac5b0f1a2bda898c472ec174d5dcdd220f0da00a2a75168cea9071b012d02b559098210ae220a687f3144dd102861429cc4f9a546190540189590d02970cd2c78a99b3748743bc58130c06383c80cc11a73c03277873ca8addd94053b17204c0595eef56e1955c903953a5cafd6ea74d1e0e9975906e156b33cb4a49824cd00b718ec1add1b4ef6467cf3e2db34dc92ea8e5117ad5d9f711b348879854864f5cac2c7b0b19b9ddb12f58ec86f6d0f5cebdce06fe60200bb71af5cdf498a14e1e9e32e61826ecfd8aecbbe207747b88a950d51be638054d76f2eac6d6d5330166d4d3708745c671991889162b3db5cbac41a1654c8dcd22ed95cbb177bc1fb9125685f803f6a285073085800374c1b90dea17b456caa6507f224a3493e6755b4f2cb6d3daebd218f6c63e8c47186927c949286f57a309b5dab925187b573fea46d9edbac646bf743ddbd32c01bfed01c00f666162885f7cf03771a287e46226821b0257ebc61cfa15c77b736c33bd940ac5feaa8264228636227f4008daea3c64dbc92cc5c27968cc5b0a1c6d39dec149a16efd22c5407a6bf2f94d63afe0910fdde6d8ca335680c1d69aa6baaf0c3680cf7c38d470fbc990645f1da55748b6bf1648177b0c6239781f5322076e9a430879a0497adbd47c6f5dcd9294f06069b62763a8fd7874132d58f0ed3ea55dbd17d4fd51abc9be787cd590837fe495ade2ff869f4210d83fa2940486ba0b9f384575d646c3f5cdaf4e2c2da3ea1f7ac539f5d53bbdfdb4de2ae9daadb76fd894a77c48ff3a3db0fed8eed351e8f1aae065d6fab8c58430ee4a97a7ba91e97796ad1b879b4eb1bc9e0bcf0267ffce1ea601c69029a7c102f3b6b7cd7b11e5336bd44056586dfc2a46059825ba27e33022f71bc6b16bb980fe46b7adf965bbd7b550f4d3348524b0358c21e975084c4a3153c91f98b34c7373030695e46a0768e7733d5f1155f14154353a471717228d1c0b880df7cc4502fb239bc7d8770f0bdd4f7990c6464367f9862b2c01fbb34caa638f949d506aa342b31dadc61a30448b99efdf027c57d818d6334cbf86d157fe8836de2c53948ab373d9fef1a9987595e38f2446cb8c95a92ae3de0997841f810066c1b3e28e86da0c5b65bddda5fc2d7a20dc9a7daaf8e5ca8aa56fd2934701e3cccebe19624b729dbf054b9e57948badd23fb00e80f71d91600d59fd2f7844e753657186aff182d8216f1bbc2e5f1ff0322240bb18dea0852ff4c48b1aec69f23e32fc00547fa3be0a10511b4f0728af08c907a94d1b4969f5fddb124bacf1c7f4a70493a10afe5f0e4d7ce6397fb547c59c375e2a0faf6556b153b92125eb191b7aeac789f142490239570f8ae49c7294e0dc38afce9e07f7de4671fa129000000000f64656c65676174696f6e5f706f6f6cae6e1f8b08000000000002ffed7d79731bd791f8fffe1413a76c03362463060709ca52fd18898955914597296d3695da1a0e80018918c07031802826abdf67dfee7ef735181ca425ad59894c0233efe8d7d7ebf3fb6fbffd22fa363a9dcd8adb68be9eada637b33c1ae7b3fc2a5b15cb325a15d14db65c4d47d39b6c9547d345b4bacea3329bc33fabecd73cba298a197e5c2cc7f9121f1f15b3593e5ad173f3e9623a5fcf710af6f432ffeff574998ff1c17f167cb477d96c3ac6e9a2325f3d8e5ea8d9b325be729bc1d8e3a858afa26262bdc1be2d71869b657153c0528b45369bdde10cf0e874c967ce16637ce2dd148732f7f0689e2db2ab7c9e2f56d1e9cf2fa3ac640fd07bc5ed225f3e86f1718a37b8a70c969d8d46c57ab19a2eaea25971351d09c070c8c11a186046c562b5cc001ad730ff2c67034f0a0437bc7b8263c68fa337f0c8af65744d6730bae633e719fc2a8f021752b6600bd3777010b0bf717e5394d315ec06b79667cb0581b5784c07aad6375d8ca7b0eb7536d306a319ca680a201e5d4ff377049365b1beba665bbf06c8978f8659095fd046c6f9648a13642b1cfe32bb5915655aaec62727f875baee772f5bd175be1819d8036be66345cb0c060604014044d9b02c66eb552e97b1608765838fb69248f8f0b3167b1dde69c7848fb7a2e9fc6606a8ba02f0f06fb5118b45de12e734cf3380ce3cc709107ce369b95a4e87b826f87a4ee7bb1c03f86677b4888e5cc4ac18fdbabe894677233cce6261adc120853207da61640338307d271e6c2c8a15ed02cf87efaa89d34c96c53cbac91738736abf64bf400bcf8872a7abd222dadbe9ea7abcccf02b1cb8eb22d9a898cfa765899099e4392028bd37846d64d331a79ea8b8c9970cfb18f14d17f09ab608787e522c098c1284f83501918f01a79b2f8126238129f44b2968eaa506301c035e5f4fcb6b1879759be70b8523f0ed64922f914ae1b3157c860000a45de57239dab9b588eb881330e726c6b246e486d5eb14c240cd8627eaa41f2051401edcff253b92cbe8bbe8529c93f888de95ef24ea1d7190fc91e8cfe7bf30e2bec94a80eab0cc9748803a66458df357cf9b70a62b60a5c8fb6c3c93f374d43c36e288f9cad1753e5ecf6098f17ac94e678aa87b55e01f30113f89d392f343da7f048c6b010c061084435ac01860075870b3660b035acb563615cc80a5003d7a681a27baca015fe1dc8085e763c0cc0204c56a7a4593c2d25a3860b6b86398837b817711a17de3c1c3c8d74779592210cabbc50818d962fa2f7a0467e368b85e0092cdee70cb3ab9023b799f8f682f0c97e961945379491b240ac0d980c8f2d944a0ede9ca9e8be1cb233efa387f37cd18f00426e3c0abdb424341d8db9a09ab6b85c3b09bf17a04535bbcea311f1f3fb3889776418206511a998836608bff81824bed1e5e514c6f6cd32ac9538b6416243717fadc6c282224e41deb3bc1ed91ac04efe0cb9e4e383a330ce438c3913e7f7f436a8183e75e1c026498e1d1153362ae2b26fe036c7381fc917fc678e66899e37b59b4c86fd52e6103d705d0e364bd5ac3d1f8872379c1560e2fc3c1c1a7cb95245e3aa217729799b13d21856c5a0178ade739715f05205c9a39b540bcb3f7d91c95b409307ea1409c468b62ac716bb6433833077220164780713805f025a03ad0a4fe95a7eab1141fbb24388136469045e962211cacb5fd55d4c0632e85d0bd6be2424cda620b1933ea5d14119f52d3c950bf42594e90455814ebe528170a1653a050a31c7b24baae2348ad11d6908d818ce2769b4f737b3d05668bab2916eff2254777fa9e61abd43f38bc054e70c97f2ec08a1a6be9aaaca6c2485b47698a88a729c10a8150189b8f738d467258800bbe8da324b04c030da2d36f4af9095b3fb2925b507dafe9e911ad32a3a3f3f38af65704b55e8607c6595d195dae1788ae9744bc127af4e0f0717461323b8eaa6c7d8fe871be26b10906755c1a486c206e209606e08c5a4d13cf49b11e0df66c75a3c7342cdbced3a8817f7c8b1f35a3ef69daa751afad830018480e5abcc601b5319996fc7e04fae71597fb0a0308cb051238944ff27541701fe6d7d96c828ff77594fb1343b96403ca3512da428f7610d30e366021ce74b40dbaf4d541e0c738a19f93118c8f2d0c006ac5478064537ac8830bc715b8d0275ce8b84b6008e22cc445956101482cf0c5d02a25a2c08d0e743c00d84d41c331f1444b3b00c6386bf4e30e4e278667c7d9a7e1935ee834bd483460476b08c1d22704f97232a15c73185548a9b80d67fba73ad41db72b8e1478b6d8c03278802d10fdab088ef9198dec2ec6b3a312840170cef1bb0cee8c8c7ffa356125b1519e49718ddbe062983f4f0a3fbb8266fc820dba14a861c5baf4008a1d13bbf2083c60e008a0d1510fcff9c83ae71d180f9bc5a57b524702cce74f06dec4f1363ca12ce679f0ec880fc489c508c4f591a10b2a001ce13c889654a38fabc93074a95c9562097e2600ac74316efaa9952d6ac404a5a27738c89ec60e7a82fb7271e93dc6465cae9a91cb0b0cfa012e4f5725b89b72cbd6297cfcfd17f3022f5dd1103694cfc0e47172626959d1bfbfc01b1ca8cf115952e0fd62f9c4fcac9c5e81fdc9faf01dc0169f949f6a73080249d78b216850f0f4bf2fe0da823a8af80a74ea6295cd52daf90735b666d45965c359ce5f6d456ff0af0fda7cecc9c9124c68a077fc7a72c2d5b527154fd00738e5c9c929fefe1c7ead787e54fd35dc2f172bb9c033fceb4732b17da878898e6cc3f7884e20b427d3ab8a07f13a060fcf6f384ce005b839fc74fee2edabb3f4e2f4d59b93889dd10febe3678067c32f9d212c5cf8d218e93f4e5fbd7c71fae6fc97f4e2cde99bb717e9cbd7a7cfdfbcfc8fb3930879dfd3a8cb1ffffefbef851a22557cb25882967f930da73350cba37191232582c9e93d1092b8bf4863283fbbc7dafc67e77f7b7df64bfafcf4e7f4f5f99bf4cfe76f5fbf1053c7dad4dccc4876c419f0e8f11dceceae3eae9eee1bfef4d52f67a72ffe9e9efde7cb8b3717628ea4627b1b76c368743c06922f8d395f9cbd3afbcbe99b97e7afd39fcfcf5fa52fcecf2e687734b598b9a3cd0c1c7649fa7226188d34abc11586490edb78c0a42c4160712778845ac4cf67af5fbc7cfd97f46f2fdffcf8e297d3bf9dbeb276ae1fec73a5b8c30d6404280e566acd5027ec0a6de249973f9dfe67fae7b3b34ba66d7d656cfee56b42a9f4f9f94f3fbdbcb820209cfdf2fcecf59bd3bf48a4eaf9f64e905e9065f8d2342bb9375958965033f4c909c8afcfdffee5c794613162f55fcfd237e7e9dbd7afce9fff552ca0af2de0629695d708c706180ef08a49f6f91c987e795dac67635ad81069136e9f8ced0a14b40d6e8fe5a0afe11db25881ea315df04b1cd73e34933ed891c2a6b992d9e65a725014664400a0d2ac17936cca4c99ca1c7b0b760501273cbd211e9a142a1c71b41b253729c0d9a3c47f072b523b381d8fa7c2c5d1e2261bc3de21c98351263fa477c58aa9cab7c01a607b7236034b2e5e9d5efc78f642721b7e4e882ca7176f52d8b67d529cf101e6bded77258738ee76fb47dd6efba873d41ef47a713fd651eba7ec3ddd87a5b542436eb8a0366079e3623d443fd4f40a0e0a74b8d1942e8c2751923c3eee7d85e05ee6f079492881b22d498e7bd68a8016e48adaf0a35396628e64899283958c7b02e6dd44055cdcc8df328593d42d815243f79a14194441615983db4731af9fe1ab733936a2c1af399c60b9c2c3678a80581d68618cbf703626bc5e8e7184f35adb32cc7f48dcf3214ec4580c6d3f7ca12ff29ca3f92bc2f2e7a475e30247c50dac700c6e35779d6033cfdf13787d239adb16bb35b6195da05ac625562dc33aff619fa64cab3b914acdc9094ed5d26778896bc4c147eb25b90d8c7b45e1f7d89553bc4090c90b2f1d6443e63f821da46c9c94c639f181afe5df68e97507c0cce40d805d0396019131761eb2e6c3e3623f703778accfc45d3c5ce1453a32989066d556728c3c2b343f3902e4f15a602615f007df56ad137866ecfd2760cf88a2a4512bf7a3406cc071dcef94f0fb0ef0846c06771e71abb09a8345db81589d4071df8998cbba20ad5ad791762331fa88f4b79429ea290c79225e3e3961f3286e632ce20d2ae120dea46d9add448458d5b1063911707c8e22863492236a3abdc4198d40f9a4cf4d63a0eeeb334dc8dce1c72dd4dcc92e08813f942a635eaa98389fd4d82a5a2cde117fcdc1d5b362d65cc7f9ee725803da70becc2c95b2b14e74bdff07908c17f8257da61db76dd3f2befc8b7cc83b06536a2ade7f4b0f78df15885af1f6dff823cefb26573576484cd5cf9b2b183ffe4832ac78269b2302a764a0b590489d0220505002f800fa104b0e2ec83ea107811fc31b0384e6aadc937f9075099c5cb80bbb01cd6b3a4273cff20e7c618b28ec256a2886808acd49f4356382aded1885b57e317a2ae46f5ae6084175a166af3435c8ccf2155b83d83ae87c6c2d6078609fa4c5a4418f349f2868942027567f68fc81bd0bbc3ba54b65d930066bb62232cdc058ec86219e0ade639b9e49aa6111fdf05428ad72367e4349b3e5d51af963a3fa1a8793ea4cf72a5fe44be6e84408f24ba370d67227a857e8a11f54483dc77daf819c867dca4f066c32f39bd51d1e5043db3e2c0434aad91a640b378895d96c1535f8b5876ec9eb218e543649e77d578048420b0ca021c80bf21c16143bc4de178a1905a1980b57124a2c09d40f90998dafe76bb6da966ea4f1af12e393e4258b2d1634a45156e62a6ccc8558a62c2074bde38ecf1a0baa447bfd4811e20d47e368f99590269c8b5443d86252b1ea947fc1b0bc15f189c43ccc46b7ccaf00c7f3e50fd258f7acf1b533958572cef7cc533fe4617442cbd24c06a8ea92da871af7257d717242f724a5355d1a20d0b9a09fccbdcb345436a42dc9d5d8d3b440cfabada8dd32798bf5a77d4496ea0c4be4c6543084d752a075eae1afc2540c65acc16129b31171f3942e648d76531f8e9d7bc3600c735026d355e1dda9756d53fcb5f2de256669198f072e4bee7a8db7ecfd195f7a557f0db895fabfbd3e2fd5988ff835eab6b5cf4ab66e3eeb2ab0924461fdecc3f4daa7cefa70da1c3ba8e686a7f02abd9b67f22ac3e1591cd578f30c0195393c874781ae9ee583cdb890058d7d867432d7b0f0378be70b1e043738e46d3a53e39c016f319734caa5437f9cf7860c44ff36f9dc07ce13b892f6c77fbc9be6b7ffc5ed59a06cafd6cb055cf1739293e59adb44c5db97f8cb255acd241bcebc829d2b7fa8f6392a118e21354a30c50d4d16c19efa21b09f67f47a53df034ec274a3d4af7ea9c9b469a436e57d45696a60014d27e8ff6af87c19cd3ae0b4ac6f3eeca06b2c3f61749083f177c6ad19c27abec86dc85ee56cc7e3543f627bcff237c0378a992fc3b8624127004f4dac0c0b80d26d7a352b86d92c7c66ecb5c7fa32eb63a11b7bc616831e9b0086da90b255a33d30b11a010313792f5c3e6cacf17eb342adaf764d59c8fa3d03f512a3f64b652a1ad7b45dc90075391828dda08aa385076f0a419b0c3313014e6b566f65ef46602ef9aa5287f31224e07e689e091c1dd71235885a3a2b97cda43228735de36b9ad9aff9dac0fa4bce5c841a3ddb98b9cc29b01c607749e18f6a4f48ac069986762246f76c05c750ebe35b9003d6dd0a3165537b5246f8a6d70cad2dc6fb2db3e2c30441a29654cd3c4d963ddd1fb0532c1cf85eea20bc7cac7919754f0ec36674145048cca5a50c5e0a4eec338d335d5be1a32ec97c9a6880b699c328c0715d465b87eeeb71dd67e64b8fbd8b7e4c2760e337256f61c40f97ee78b9a760232b8363b2c967e5987e4a9454fcfd8665e3c1b81ef9a5c61c3749ab8a5b86e77617909541db8e79a55701fa86f8a6d866b97791c342a608b08e2c15cbd3c7ca17e59a270714b3b1022b203019d1f1feba5ed02580ecdb5ad4ad6d3ed796e598d402a8c36fadc6817c5dc16fb740b5a67994aeb0a9c62fbe01d831f898f39dd14c0e93d2305e6ca36f3e7754db0f396c301e164768cc2ad5d910173c4f008c3f802e4b11740d687429efc1979a7a414e60324f5f46426e80aa18121d2fed4c509e36a3d2044959c96f8ad1758b479ab393b8c208893120ce68c5d2441d87339c9a9c47851b654b49ca8fa31f8b5b802a2026bae5b8f441c98cb9a12a4815ac7d18bc8086d6f5620adb9f63d4c6ed35250389d5a1570f93cdf8848fa27cca1468b66615660a46d9d9ccbb5af82a0763257959c5f6019e248e28d5c375a5b3a9006e647dc35be9d8808592ad383e100790ea4cba02a306396cc7d3f2669661ae92151b8407c723819a664c11e27f4bc4f030c4d01cc3942fc1e343183260a8ce2d3dcc626be0c8e824288c4701f1b1ef9665b8aafc12bfa5fb8db8f8d7a21c26dce20a5a7b9972b527a50953897916a9586633b2da72a8a5688e6f45fa5f40b10bcca824d68be64c230491e98eec797a9c5935ac071a3a2f12cb0e4d123d8bdaf61af1a7013f1ce260cb5ec7c9711363dcf561e417183f1b9e403d85bf01548db93e44f9ac44ddb3cdf54eef87baf8198f35c630e15e727fc2b0975b38f246a24543229e26696aa0490d05f137160938fd75b15c3d1a4d97a33546bb4d043db1b8224a03c220fc45c13cf39443c8741703fdf95b4f9f12dae01d0ff9fb07cbe84dc07f6a6ab82918acabc5ca13630cc77fe9b7eecbc72c135e40dc8cc78c017204d24217a7a5833e144c2a588ab1388395706d21cc5e04baf8dc237afa863a92408009f3c588f072dd1723c16062ac36b3a51e28acff7ac345bde91984c7395a299e8a71ff738da9939c83ab5c53c15126a81a02944c5f36b90f60481036ccbd414aa2e5e170d042ac0ec497710216b0992aced90549465defe0d8a2e56a685748d32da8d2d3510d00a55e9ce29c36cc64b899be60faecd877ad286d45a6ec868f04cfcfb999800e2840240a66eb1b8c5b4d350745187e7c65df5973ebd0e2c1f448feccaede3058b636b4e5c030cdf6667c8a2b620c3c75be75cfd979c48c4a617f799ed291c2fcfa83fad3bde7304785621576328f0d41c14b406f956b472bb01c10b8c91c338a65c905e33d692376e2a803628be953f72bb3c880cd26c28dcd8ba55a2e69ee5ccbf3d42308607cba25920b932aa7f11f9e4662b0a0f974637cb91d19f1f14a660efacf4b388b55f204abd460500d960e18e4f2fa401c873821ec22c698b0492dcbe2366ba8cd2b3d6e529319391169f7c731551c9a8f695672c59fe066e8d1bffd956a56225b34c0c26c27f5ff55055cc1e13323750ac4f399ae610ca20aef974f82ac624b426df927f7f010071377e12607506277622d81580f93aabd11b8f7c662b6e0282258c3e02acc9b6ee532490dcc734fab75d31781249f2f9f91a132a2724f634bda6f55d3b685f5d5565fe1fa0fe6ae8c40871ce6caf82ae390e9a89547da57e649abed86be16b33a02d3466fd0d63aa3584d632c6524becec0968929f576fa1d0e69d622722c8ab078197a96da3b0cfa13ade0044d0d96ab4aa9de96792cc8e88359c0d1d75fab0841918a8c312fb769998361705c369ad13343e7e6ec10beb6dc93b68bd94528eef146ee636252ab2ae4dca4a37fd7148caa6cddc1c5a2e5ea719cfa96146ca82843eed66d69e9482978e2f156e38624ea3e600f804c15019e25002c0a3db3987915d1d820b1948a60640bfc4c822850f08a98a6e76286d0fa43c31b672757ad23161985cd1d33ff73f4438528e78ffccfff6c412e4d65216e060e716fbdc03a3a0b3f3491cf7c9c4fa38dfac013d7b2a7d5265b4805afb2520b49324ea6aae60610c29c6ab4a9d3c628577dc26be17f1adeddc09559c4f9a83a31cca396bfa7c278bc149a5ad598ac79582044b2e026777197fa34382665e8317ce4e55ac6140a32c1186a2ceb84518edcd9f348ab1c026e710c4bc27263a58185db6086a5b1505ac3cadc16e7e0c31c81206b4d697b75fd31a9304dd8b36f63a910dbf1d2c8d3a71b89c4e78581fd49ad68865be43b8579c43414771528fb63eb76f6634fdd8f1e711231f5b72736d4f37294dde426e089d3e818e706587b146d9bd63c87f0c43790d4e5dc011cc59c2f1a0e8c52a88c5587aa7c9110f64dcccd0d5bad5b38b4ec15a138d31703e7ea92bf74396b5c042b62ce66fbc2e5c3931a9e05779c5ab7176572d745196690027b1a2a8acccd73b8f445d95feaa3febac032c2feea7f41f3fc9479234a5e45e20b9bfcc54cdb1b2889ac7d8b8631c4afbb5ceabc41efe6b5ca932d787f36232d4770ab2b5e282284b29d554d6d163aed2ce7524451547b76e5e822791d6b13af4aab94ef2a2fbd7101eef96a7a6b58a7056dbe41e593c43fcdfb8a140c9972b631e39878a68b9f94d2c85b9eec1e7fbe8da415e35b2ddf65c3d7f6380a71607fa01e8cd63365caa01d8d97d3c94aa85956e5076e49627617a6fe81232cd5b0d6f08b99d4e6b1cd7ce1108f3672e3eb5ad61c2b7f85347b964d28e304adfa66ac428b561b92bb1feff295e75c404a3be0d6c9cf7058d275c2ada0d0d2b212788491e45c0b5f713f53a778983b919e40a73e6d286eed53d82c258c4de6d11c95b71ad3356d180441108dd7ccb2bd5e1573a0bd915e3dc3d12d83e97f8c90394239797ccefda49696f9c3ae4aa6a64f66b36fccaaf6d12d42872a4d7842b9a904b432c04ccbc8216b796c9a7d349be1c5a1e1ecdb434a7093f7460cd5526f77e50c12d4550988412e51675bde917c9c225813b12ecf08c2e7512dae6c89f5dd30e7fcf5abbf47b242f8b6988308ef4a2cb40d341ca184861f193fd668fb58c407139fdcb0337c0d87b1dcc42c48910a7b89ec7037ac5b6687f3c8678de38f8c8b158c28bfd36be9cb22f09e3aeab28ef014cb608fd7235d97c52562114db08c8a1259189637caa7ef64b02b0b9d049b6b399b5e5da3f17426ad7e7c0cb404f03029193b2a4a92ca085279662c0835032b6a31a6f8d6e2fd746edefef0f43c4e2443c59651ec95d77d49d73204c311832ea7f422ce172142d7b47673781b8d9eecce94ec8fb699c7777793f1151572b16690f67a31fdef75a8f4ee2d290c4a6cfad4f579c6ea36c9c15505c40612012f8a203cd04d75fb03a434edff5bba79509b07419ebafac66eaafd90b4938f5eadbf771d6cf30c5cb5f064257e1268a5e7f851843173d2502a1aee800b57b0f56694cdeb2dffa992598177ab7cc16ac03b8983f570d293bcd7b4a2d4b99289ed12320a0724c5d05331c177f6367f6cac966be029dffa34d7ba43569bc21a13782ff794afd0de0ba0571681c90417a6656d8a005a15c259d9108745afa9140fa7a3ccec36bb63d98833ac030a666e3474ba1989eeb95b4f28dfadd783d63c5106205501c42e6cb4473c857182e80cd64214dc5a22ded809fb18ce58c55bdd886a59fc7cf41c4a0ad52c395abe0aeb6cc548dc082ea26e57d4d48257c961b50cb0c6b82c868883058f5eea43b37cb202f6b12ad6d81d89cc74866e3be4a5e79912a88abda24e241dd18ad10cef141148a4e0c58143126a6ba7aa5df2eafe6581f76a0d5ec2e840d751bfb7d96b71e64566756bb68d9a7f528a336510f39d3f320bb8170b5e49dfa8900976787cf3ba988db1980396cb94e3cac2f097cce26b4408eb43a3d8e0cdb684055aca2d3b75ada2b4341b97d5201eb11a411498abae17802c8af704821a0d8bb51fd7545882dabba76a9cbe6bad689b375d4a7f96bbe5b93fb76db9be2bd988ce0db8834f96fd92f4ae957ca824b496be3f5fe517433dc153d35a3158ab220f0e5f98ec2ea1af65ef10381e00fdadc3bb99a404c76bb69e05dd083ef96cba8d14286a546cd26db54ffd8f98a35851da2cb4255cf7bc193263f26807bc0b5b3d3b5a2cf7cfb955812ae62bf41b8ed6db28b39b06e2b40c3a70e4e2736699b179849eb136b47b4ca822af9cfdb0c92ff5da1cec1a8e3d4417ebf9906558abe6256c3cc30dc63b7a5226936212dc0a06f6306519632f374c2ec09670c26ce5becacaf7c42b9ee9d63b612fd4566412b1c7bccb33f43944a413953827b53d63a588b1ddda8cf50d602e29592d591bbfda29105c55b5d2abc6b10fc31cd145b3803e4c1b15e958b62caa2be6e478b71995e09049082e22a3f1ceea21259542d17d9021bb28fbe73169d97db61e39ed5574652b7b07c747cad884cc66b86562ca3287dae0dd984aad17a4f1e41b3cbc58a408238be8d0a615a648afd729cc8a4c7dcd9d2a2085153e7240099f57288ad958813d4db3067e5a470dcc52172497a0405a1c5c4762fbbee6d5d92412cb30393f1a0bc3818dc89e4507f0d4593c80f960abd71204aad62f2db36a037f8681c19dbaa2f2e678c5ce7720cd1041f2282023ca8c28d1dab8970c1f5c3f25daab57aadf4bb31b5c799baed35606c2eedcc019f9e09c00d724349354f43d0bb187eae5386cc1f14e2b0290c17a0addc721276cb1d46edb563f31b34e0f490bad0fcf15366ce7f7f3956c7f5cd1620041b7d9c5a7cb7d117f6c960f65c8b2ccb1b846a35a43b72e281e9b3f718e129c404a1f7e64d9bbe8bec8fb29605912e56b935d672408bf08794ceac7f882154077a75a00f2c34481956f26a5a2d18d10acaa89ca849456f15fc764474f170127d58d9a2b123934655b8e0a7c71b596cda38d3ef5561be716d740a9d1906cd8abdc8a3abf150915753a252b7ebb21a4c657bd4e393fac7f1c9353d8f55415a2e104d7f943f03d79c0def2c0e2b2e08dc3ab731fdddce9a86987b7492204d10a2a68ae15ba543d28b91a2ae94d8651b865afcd22038aff5d9cff745661bf248730f74c97abe286b04cf7dd2e8adb9619c2bd2ea9f3cb947abf97f94da62a2e85cae9399d664c478e3e3eef0b2efa66f37e95a10ed90632b99e5add1bfb2c7cc4a64c610e74d53e4583f3309f158b2b62fa1b8b3db8ce6613b39fec3aa7cea64569fd10d08d3956d595165b2286898f75e21b557396695876f2c5761e7334a96b631afb7dca5af6085b98d3dfe83b7b24f3f110f6215ea1049315a7a837b0b687e6171bfcee5adc4b1093bc02042335169a9ea2fad2f0d699eadc85dd5a3f64d67d5585a23a8922a58f2360f3402aeb31c696ea2020b2450e00626dded6bc43ee6276278618e6232cb9648483ac17984288757597b9d10a96e2a0b1365a41cd0ab5ea67df63a31fab9683376445d67ed0c391e0cb724a1da5ee7259c7e6d22adbe9d48770823850b5d66b99b8974fa33f81fb36a58209b6e13ce0f8371be1b04aac7ac5549fcac2ee58028bf7ebd818f4cf8a94b35617a2aba1d6b6f0d2cf00f0a71dd004d5a1b8d4b3f1786a7a494327e62134fdec6a453b8675cba6ffa07dd43d7183c1ccc37715111b0db68b90fb0d5063d3d93848a2a2383c11df5148456b79a2b92b21612bd5d8ffb9acd0994fb41ece888b8fb03e28b688d65a56635d51a3b1bbe0ad72169dc7b650439c5e2d74d5194650d1770be6d4e5b175206f6eb2a59b3f4235c21c052e54a8b4667ef6e61aa5f71549b46dad87cf2f49005574856a02b7b48a64548a4c4ddcb29ab536b446b22afa91c920236a92671daeefd403b2009ac442ab7bfac8e8be67707454e46ea71413c35ffea624bc1491a02c16556bffaa5717352a7f82e7597a3f0ab8ea1b1a02a9188616938d46eb394197e5e45475e2a4b51ca620994ff63ed972a9c142e09b54e77a5ba8e320dc52849898ca4e4c2b268d6d47158ef01e8c7a59692b24bc61d5b42b35dca580654d4bae5fad64639c6e6bd3d96dbfb150d47bbdd24ddbad78c3a9f02ce78d55edc9ccc6a12deef99a46df387dfd82ffad850c792f30b52f61c2cc24a1c42f22150d42317b0651415a3eb9e1132eea4d6f66f4eea99102cbb6488fdc00f2aa0e7472b62a23e5e6346ce073b193394c460bd7ae43cde832e0b679796de8d19362594f59b31b7a6dab1f07da7b799dc47ffcc70a8cac295e1a59ec37b87c4068ac8a329d2cb3797e5b2c7f459b2b2b25bc6657dc278177595fede7e7af2fce5e5fbcbd48ff7af6f734d6bb3202a8df7f799cf5bac341921c1f8fbbc3acd73eea8c3b592fc98fdba351bb9de583c970388a47a3deb0dbe90cbbfdf6f1607874dcee8e3ac7d9517fd26e4f46fdee51b73f3aea1ff7f2bc9f1c4f46c9b80d430e464932e87ef9a4deea7e3effd9b3ba6c30ee8fe2499c1cb527c938eef6baa3e34176dcc9baedc160721cc352fbe3d1d178d8ebf506f1b8dbcfb26ede1f8db2fce878703c4c3add7632186647ed5e32898f8f7abdbc3fc9e09fe151274f3abd49dccd27a36e9ee44378709425c3e349af3fccc6fdc164d0ef4d46713c3c1a26bd7c188f06bdacdd9f1c77c6e3c9b89d74c0ffd1853987fdde71d21ff447933c1bc0a3c9d1a43bca7b437868928fdb5f561fced9cfe7cf7f4c5fbcfd855ac9c856eded4a98617dc4b73fa7cffffe1cbb229e01085f5c88573b7dd9353df0b653a24604fa30b3aaecbffe64ab41cc9793273bad409876e566423bd1d209808053462054d5b5d1742bf7e023fccb278c73691ad9043c308f78331d306d990702ec8015e7d11c1506e9e9b498a7304c4a2b1dadc1ce3b6fd859d6e15d68bd0dc5200d9bec45192c238edc7d4f4c6ebdde8ae2769bfec1fff85008d46a8a218ff547dbedbd362116a3ae4e814da94b0618c8b0963c135b966b98d799f77e070701ea233273ad7092dd08198391f52a4e201e59630c56f5491f4d2b95be58cf45d3146b4a7f3d75ebb17705de484005bdcd972069500694793a9b821aa5c7a054b454e27f2bdcf01400b320dbac6ca0193c1fcf1999f73fe378acaff4d3b15b1d7a0ec7baa906cfc61eca773215cf6807633e55712e2d9f2bc9a1fd1a54bb13f5d043aa59814224f191876cbca114cce994fe13b43a13b236baf30725a7d4bfdfbb6b8b9ab9b23aa57ccc0a89f883a406666049b355c319d29387b88184dc21fcaee98a66da7208ea38ebe9a96cdd5254513b4a0fa5718577809444b01f18d630eadeb243079caabd89223f05ba55b1274589519865fa6b7ea76fc8ac506f29912d5b6fd337aad578f334aee0d30341aef4e9bcf58054fdfbe0c27cf572ac809a00090416e63e189a7cf3a4929cecb91ca5c5891f311886cd54e0e0ff9f23de1572c1b7edf771d269fe97df264aec45ef4b9502ae397d88b610d921beb417e7a8c75c0fc668f6a3f2c390a81349b1d14682113becb0415ff384626803d8e75b398ee216fad41a24f0e178c38cd54b8e818b24f654f622ed199303ec314ed8c40726b33ffe034c46e41e49317e1dee038d0c0cfda0c41463b4a4b4dfc3d5ac1d533c14338d3c8d2ef2d9a4924a594a8541ac9f0861ea9622dedc54b77093ff2c6426aa85749fd0d17d9aac75df13ac4bcb0f7a90c77090c97607c90dc6c8c44b47381ce428773d99da42ebfd97edf6973bbe187fd97c2095a46255bf2b231f8d32a2b973fd3db377d60a9c552839bdabbcf70cd971f7b6a5273fd4b2bdead625165fb38dbb3bd463f3249f5af7a0ae8d7e0d75b3036a83cb1df6035d21a28a97d06b810d2217c01bd56796f3482d84d93e36f500b267a54370b1b4151d795e7616187cfbd8f3b6bb97e0eb031f7e6f6e3acd02e4d132eae9d5c690425187d55dae2dff57df2464f7ee4a490a5b78be7d276c5edf940ff683d103ec59e3eb6d7a5edb2b6c1e4c52680505d8676d807aa598365afb1d4060e08fa709c5bd08931a46fb6dedf6ca746fcd695917358912d31c6c34fcf78022ed40b268c76637b58d82bec21a66aa120505c9607b27be4064db9a1be791d3e9b62d2963f310a9d4b8113c72428f44df46f1570e3f0bcd0abe3d8d0d6d304791637d4a610adbee5caca3d62649a27917cc966a067b8cfd7dfda8e5c2753efad508d5d22d8e5a0b4e0bb69e0eb3b6add17aa326e38fdb3163fc8cba763fcfda70dcee7859749d6a6b2d3255785542d912f1f63a5b6946eb31bc514ef552fda47568046445a4f84351aa6bd988addba3628f4105ce5ab841e17605cbfeb1aa3c064cd24104091aa3f91bf67aa825a3bf11250601c99045ccc3f928513869270c8593cf1c851f147de336c6311144ed073e42a4d696c3523ea3ba2be1cd18c213f4daf5393c9db4f11126ee8d73e6bc1ddf3bc57c7e8898b43b5b216216cdb057f75234f8c12c66df518d8a3986ef8fad343ebdee52c521ed744e9ffb5175da7d7154a1d37a10439d62e1d8c568967f6206ed2dee42bce625fde7e3bb0d995dacb40a09227cf7194518cb42d05a2b29c4642a4ab6bd3adb764c195e4b5ac3637b0b0de5e9f6690baea4d7f69a9bb0250e2f3da577c471e1866a050ce2a82fffca9785d3671e648a11acc15afb589d094bad887b5d0d07b7c1ad486d1ba16c630aa6e9d6b10099fb61daa3be2111da546a261533d94b200c0764b98d7e601e8c5c008b72c77ac02cad5eb42da242082c97405b8da532ec78bceee1b2b5b06c311d24ac41a1caf9b7da14ca8ee6b5c08f533b67f71dda636aa29a6a424719a054355e258cb00c0014547b111bc37d674df080ac3caf6ac54b4621d38ec3ede3eb320eb900f7d6c68e6985a2d6d33d92b236cc65b65d0cdd9d686a20b98ff7ec8e1c2e0048a1569c4536e5b65a9ced13a3e451876cd50e4e88d32debcc678913d8dd1450347fbfb2d0b116cbf6dd1471c29d2e8a828d26d6d1ecc0ae424429ebbd52063fdecd15bf62253d609100a98c3037c7de89633b4530936984866a8c838a7e5ad355090ee8bd28bbd776297b5ba29492d1c60910d25ac0fc21d123dee126bac70e3bbd03682c7d9fc6527dd1c00598c7df409417ecbd099202966603d34c43da69a918e7b42356c877fd38e1a96b2384d6a290724bef7113e210dba240d2de06f2b08d0d9087f10e0ff9e3e43808ba38222d06bb2dacaca94264e283d0bd0abbd8e71ddd68ba0b68608e30cf64e8b321d499e28155e5b0e84940d5b80fbd6cb7131e847db90feff09c678bbb4fcde37998fbb9e1abfcdd55b9ef653eac10b25ac58bcdb666cd40f75bea7f413ba1cf8ea013bfa78d813540f77867b92ac0ed6a5b1b24197a540c49a60bb2ed55a27ef0d4134715fe2e343bf830125eeac15e08a93df014c02afa3e6ad000dfe15f68154df682beff164bb3d134ce74833d4d54831d4d541fe57d6737c3c1a086b56fa793db16615dba09bb10363b2977f6513217e5d6acc7baf06c0fc24e777010bbaba4ff3d8ea2b3250b4bba833007634cc6d2c4f782542ff6b20974ba56ad22e96db18a1a803eda1dd0fd24a90c197c283593b96f0fe8e9f9c4b4cc8f3220eea350328592d83eacd570379bc01e64dad9c37ed69681311eab36eb87c37b73f8cc0ce43ca7c24330d0f7b00ee13bcf16cc24ac3a624566213ffcd91858b12f609236e3a283c10e905170315eafdf04ad46a3d0565427e6be4637349f4b8e225d644f076ec7b54f43582996c515ce0e9fcceeb48201e2f54d38bcaf92967079d71d6cd515f46300f3c301c947e73e18a50705cb1f3c33b477bbab0758cdc6bb63b5cad33ea4cad369c7bbf252f9aa4b8705161a63aba71acab96c6a849c923358cc47543149605e6db06440b7e336f53f01feeaab1359235a6d4fedbbc7f4bab8bb0b57ed49a530fe0469dd93c225cb7cea95615c3bae2ff1217a64d5cf328de52e59202ac9524fe6abb2d0938e760166cf5d1ed4eb2e371a01e82e0f24bd5ee221bdeff0e842637bed3a3b61598761496f17ad86bf2cdf57cd69a7407bab6c7e73728285b3520ecd9443af113826b4ce98f0ae7f4ec2e8689d1706b0305ae51d53be3021dff1435ebbdfdd1be4bbf84fecb7a8ee6550ed6d6970b0ed519baa6372c4a6a01d567a890a64eab61359577fd7f4390f5c1cdb0cb79245c44086acd83df3c5fb59b951d1771fb3c181ec2b06066c4d7be2659f471e6fe67aa73486bab233daf08ec5602b2088276b22c9c6c02bf742287222ad95ca2562a900b3759bbbb80a691bef1ff547d04c0e70a6ae41c9d370560595b10eb7636255d4c94a8f1afecdf44edd37cbee3476dbae85c928da9e5e54185aaa1753b5b72717e134b9df26bbd46d77eef49e774924d0071815c521ef48ccd89ab1c9b8b602d8ebed1df54a98d4eb7ef4ea9b8757c3ec21c0eda81b6ca117ec0d7622623f4be59708dea454b4fae6b5a2bd2dbf59fc0b3eb55e157358c688320404d9ef65a6d93eb2b9dddb9bc7c93445934b50fce9b414fda17ca058e637b36cc4ca550303998d15c7d7f33d3e4e2c4f5c9c18afe7f33bcd78fecf75b9e2dda0a95521fc3acbb377563038ab354d1020ad4edf7ad8da6c7aad02210d46f1409aba0c04a27335839e39448ca8c3d14531d132e5e5fad9b3da8eac7a201b5948f2902cc44d9e6037d0d2bd1764d49701851fbbea914cf75585df56c7ebc58ad80e44b389b5b5ebe2367f87cd3ded4c06b53b750cbc41618b91ba8c5195a4329c6da5b3b593fdf959a77d74207eb6830ede8b7d2f7f04a642b3a14e552b28425b71d75c14b76e750059933edc0acaa859b997e6668c64697149b319881dc4ab9f417e5ccd24cae44939f3f99a164dedb21aecf643ca1e0b28bc64828843a5e980c1d64f1f6ccbf7e6d58da55b57fb30e11f26d5be5e5f138883bb7ce3aa2f938fdc219c7c521552e29a2ee178279f70bc71fea4e6fcc94ef32715918fa6e92c0e240f984f2561e7b33e52a5f6eebe916cebaf76e16b8f30d82a202b095e89066e24e3068d89df92c80ca725feab25dbb79e30e07afb4281c7937507be5c2ad4d07fb36baa42b39adde73cbac076e654fbed6d0497bff3cf53660cb52f46f254be29436ddfa5b5c8a721ef74d071cbbb984dc898049131d91519934d5e3d071b95a91697898e49135ad886c8433eaedd3676f9c8be048466d1c1fd21503208243adac4797f17406f83ac5addceb7bf212b73f0a12822a9f4ee75078ec2ec5a726d178fb8ecae17c2c1335d804da3ccd1c38efdddca5c6f9e7c2f78a10ea3fdb0cc32b93f5c97bf7fe7e9d4dade872724153c2171c32ff40beb5e9de11ee0c4c33c5caab8f5cdf2f6ced39d69fd37b704c61fa72970231feffc16be802dd886e1262ab6e1d6cc32e66d476a8b9814ac2d5ed4c32f762140fd7d93988c91dd177d0ba03a8b0e83f22ef5a15981e48a58b3c63053fa2336b6d195b0b9f7dbbeee71abc14c2b5e0ab221f79d4302efc0b90095b6203b2ef333aef5f4d11b75c282325059edde43c385177bb79a0c2aeb34f6d6fed37609eaa91b39bfda9c35dfdbb6b8c4fe21d8078048dcf3c204edd95a2c3a5524d9c014eb8755efbdf3ceee997d89beef6db2c9360435c707cde3eaf4126f58b2a049569a928a1964aada332f2ef871462127edceee51c8e265f9bee70e4a7de0aff34d68fa3001a526ea742a5007d58d0708cfa5708db822c2cf402dbca2bb9dad1b9e400cacb3c1e34f9a0f1b80c238d7be42a56fe61bdd7f254beeb2234ce7e7faf956b3fc88ab651ca4fec14e28176faa73a43490741b0de4e1f318e324debe3a55754634b2432d41c8acf04b468e40c1a79d969fb4b7f547e13bf7841849c72ff4f18b0a88212b4c6a9706eb760f3654af7fb0a1fac7071beab87d88a1763abf81fffccad1753e5e83fc24858c276380bc605212e5e9bd686cfb53777bc055b6c1c01351173650eda262d5d6a670559bd4a9c1c035b51d8c4c63d445b7ce898c372a816d76bf0ea8ad3bad34ee0612c5f796357b57f5df45850dec6713e4e3ee43433e01e6b8fd4af1adfb112d49dced6beb3994a2bb7d01398ff29bced7b3d5f4e6ff5ea58fe4f772721b2b7d6ccfa3b4e0f6ceb61d1d3a7622026bd9e0640b76a89253c26a7f25acf497ba2e20d152d5b1c1913e56c7a973f62d3ea20d3138c2d7e2bd4a5e7902ac42731bcbdf3c77edda1d7b18e53a3e59556d79cbb045d835b5fd18e6ab5bf4a2b97e1ce316c1942f55e39c8c9bbbe98435cee320f99d497be7cababd76b0b02e093df4d0c96d0044b22565234d97944669cb9ec342a7e3d9d64ed0e9ee0e9d232f740ebdcfde61f6d9df3dbf2079907d1e1f669fc7bbeff3a8aaba28f545ab9429d8126011f13c23f07958a6fcfb6af2149635bcb5196c0cb935fee73bfac312368341dbbc2479387e03ce07de86479b3896fc8bc4d77133faff95952b379e7edc11c4645fd8bcd2e738b0863d65d0605b7b945261f56204a219135b20feab5f237985d24df7841a208be383504cbc7b3db1defd54fdabbc0738d11fbf9bc33f6273f8218ddc9faa2dec9813aa650af357928187199fd03451b449d167b69f6fe3580367ac84579275c7ba3f131cad2539f2ac25d96d5f9d9e3396a7524e430b896bee7580dd8ed0141dffe7f4ea0a73f840f0cbc47aeed444bc2b26e45696f9e5b613742f24dc21edbebbb7d7b3dfedd6476678f860c8dc1775766a21f3a6b1fa0743c67e3ff18cd5dd3016552ac84d17394f12855730bb3954ce8bf5b786ab167cb9d741f68f7985befec797da59ace1ff93b4588ef32588f6719ecf7fcfecfc741b4afc9ed929237d3b5be76c76da9b6bc82735064e360c5ca39c46cdcc155b773bbc4fcf0837a4f331c2b7b41823233b258af1ee6ed68baa9f031a7b8b8fed08930a687f3ce04a1c70c5f5b314f70257fc60e00aa6f9a9b67ab759a907f9c2934ab3ac0bf661b1bad68da5ead202e682f17432c9d16a642c417679d6d27764da01b3b332a374550261adceceb127a1a3469e824c53da2a51612fbc4802355a379700547b8c9ef9cd2675464902a37c4cba13e5438294c26e209f95d2948ed625dcb11a0e2c29203de67dc67df42e54a5d878d690987baa4d1bd5854d29139c2fdb29b070831c15f3f9b4243b061a4093f6573e20d9d964a6b718f6fc0e2c2370b827f9fc6675f7c3faf859a3692dc12abd24bac8abd676df9491c0ae03b786e05acdb240437a0a8cbbcc17e5ba4c7fcdefc2976238da8bb3d7176f2fd2bf9efd3d8df50f7e3eff398d7fd7476b551ad9a58a483d9d34d93cb8afe0889b652362f4dd4cdb033719ded202de6957f4a4135d46f9fd7d0b7f68edd93bfef6f6e8b6d7830a80651c2214a8efef66a6cf745c6ba6cdd51e60d063ef6cc91eb32515e1077dcf196e56712dfcdeff4893764f78a4593fb7e3603e083b563437f943160f74e83c0a211e3cc8a9c7fdb8168e9125510bf5d6f67d28a488db493711e77034d80b31ee2faab576ca5e3f26635f3208385a7b2e5e257e00fb100bdb5e70cde01b6c77315acfd733ca4517c1361431124c627f97b32cf66c5616350b8fd4c93000affeb1cb425c544aee1795922e059f269d638bc3ec94d7adc15a42574110be29a7a5a8e43d5d895ad2605a9e8ea8368028171c95734a5e577e3233330e15cf1bf063c01d15548ed923446251c31dd3aba34b79b1bcc4c7a9828458d8282bf7ec46d75687b7bd1653bf3da26890d86e6f6c91183f04ff3f8afb7ce39e6c8e76dfa55240f30d31ca8bc2b843202182c8b0022bf60b6c474e69935a99abcba8ae8d552baad5111758ad59e2185c066f6e28788fe128ee9895ed5f12d632966324b81ad0ec1edd53360efa0b756818d6811463068d0b3c5edddd69dcfbb598e1241253c967f40578bed643254ea275bfdbd2ae90155ffa832db48782577ced264f5629787f9dcd526197e27f2a4315ffc0570167a3f9aaa21c89312db558d7f6216775cc58d6ebbe9220d6101e5b983588235f3cc54dac411d680874faf0bfe4b5f7dc584b010000000300000000000000000000000000000000000000000000000000000000000000010e4170746f734672616d65776f726b00000000000000000000000000000000000000000000000000000000000000010b4170746f735374646c696200000000000000000000000000000000000000000000000000000000000000010a4d6f76655374646c696200").unwrap(),
            vec![
                hex::decode("a11ceb0b050000000c01000602060c0312ab0104bd011005cd019a0107e702dc0308c306400683075010d307ae040a810c0f0c900c88080d981408000001010102000304000219040203010001000400010000050201000006030100000704010000080001000009040500000a060700000b010700000c000100000d070600000e080100000f00010000100901000011040100001202010000130301000014010a000015090100001609010000170b060000180c0600021b0f10020300011c010100021d110602030002091405020300021e0615020304021f0f170203000120010100020d150602030002211a010203000222141b020300150e170e180e190e1a0e1c0e1d0e1e0e030708000503010302060800030306080003030206080005010100010800040608000303030106080001040407080005050302070800030505030303070302050302070b01020900090109000107090103070b0102090009010900090102010302030302060b0102090009010900010b010209000901040505070303010901010b01020503040505030301060b0102090009010106090102050510706f6f6c5f7536345f756e626f756e64056572726f72117461626c655f776974685f6c656e67746804506f6f6c0a6164645f73686172657310616d6f756e745f746f5f73686172657321616d6f756e745f746f5f7368617265735f776974685f746f74616c5f636f696e730762616c616e6365066275795f696e08636f6e7461696e73066372656174651a6372656174655f776974685f7363616c696e675f666163746f720d6465647563745f7368617265730d64657374726f795f656d707479146d756c7469706c795f7468656e5f6469766964650d72656465656d5f736861726573127368617265686f6c646572735f636f756e7406736861726573107368617265735f746f5f616d6f756e74217368617265735f746f5f616d6f756e745f776974685f746f74616c5f636f696e7307746f5f753132380b746f74616c5f636f696e730c746f74616c5f7368617265730f7472616e736665725f736861726573127570646174655f746f74616c5f636f696e730f5461626c65576974684c656e6774680e7363616c696e675f666163746f720a626f72726f775f6d757410696e76616c69645f617267756d656e7403616464036e65770672656d6f76650d696e76616c69645f7374617465066c656e67746806626f72726f77b7d68860645e7aff66895b2367686f75871601275245949da71a70befe6ffb1d0000000000000000000000000000000000000000000000000000000000000001030804000000000000000308030000000000000003080600000000000000030807000000000000000308010000000000000003080500000000000000030802000000000000000308ffffffffffffffff126170746f733a3a6d657461646174615f7631990407010000000000000016455348415245484f4c4445525f4e4f545f464f554e44205368617265686f6c646572206e6f742070726573656e7420696e20706f6f6c2e02000000000000001645544f4f5f4d414e595f5348415245484f4c444552532c54686572652061726520746f6f206d616e79207368617265686f6c6465727320696e2074686520706f6f6c2e03000000000000001245504f4f4c5f49535f4e4f545f454d5054591e43616e6e6f742064657374726f79206e6f6e2d656d70747920706f6f6c2e04000000000000001445494e53554646494349454e545f5348415245533f43616e6e6f742072656465656d206d6f726520736861726573207468616e20746865207368617265686f6c6465722068617320696e2074686520706f6f6c2e05000000000000001c455348415245484f4c4445525f5348415245535f4f564552464c4f57315368617265686f6c6465722063616e6e6f742068617665206d6f7265207468616e207536342e6d6178207368617265732e06000000000000001a45504f4f4c5f544f54414c5f434f494e535f4f564552464c4f5729506f6f6c277320746f74616c20636f696e732063616e6e6f7420657863656564207536342e6d61782e07000000000000001b45504f4f4c5f544f54414c5f5348415245535f4f564552464c4f572a506f6f6c277320746f74616c207368617265732063616e6e6f7420657863656564207536342e6d61782e000000020415031603110b010205031a03000000000d380a000a010c032e0b03110504240b000f000b0138000c070a07140c0607070a06170a02260416051b0b070107051116270b060b02160a07150b07140c0505360a020600000000000000002404300b000f000b010a0238010b020c0405340b00010b020c040b040c050b05020101000006070a000b010b001001141102020201000012220a00100114060000000000000000210409080c03050f0a00100214060000000000000000210c030b0304180b010b00100314180c0405200a000b010b001002140b02110a0c040b04020301000001080a000b01110d0c020b000b02110e020401000013420a020600000000000000002104080b0001060000000000000000020a000a020c032e0b0311010c0407070a00100114170a02260418051d0b0001070211162707070a00100214170a04260426052b0b000107021116270a001001140b02160a000f01150a001002140a04160a000f02150b000b010a041100010b04020501000006050b0010000b0138020206010000060306010000000000000011070207010000060606000000000000000006000000000000000038030b001200020800000016380a000a010c032e0b0311050408050d0b000107041116270a000a010c042e0b04110d0a02260417051c0b000107001116270a000f000a0138000c050a05140b02170a05150b05140c060a060600000000000000002104340b000f000b0138040105360b00010b06020901000018130e00100114060000000000000000210407050a0701111b270b001300010c0101010b013805020a010000060a0b0111100b021110180b0311101a34020b01000019420a000a010c032e0b0311050408050d0b000107041116270a000a010c042e0b04110d0a02260417051c0b000107001116270a020600000000000000002104240b0001060000000000000000020a000a020c052e0b05110e0c060a001001140a06170a000f01150a001002140a02170a000f02150b000b010b021108010b06020c01000006040b0010003806020d01000001110a000a011105040b0b0010000b013807140c02050f0b00010600000000000000000c020b02020e01000006070a000b010b00100114110f020f01000012200a00100114060000000000000000210409080c03050f0a00100214060000000000000000210c030b0304160b00010600000000000000000c04051e0a000b010b020b00100214110a0c040b04021000000006030b0035021101000006040b00100114021201000006040b0010021402130100001c2e0a000a010c042e0b0411050408050d0b000107041116270a000a010c052e0b05110d0a03260417051c0b000107001116270a030600000000000000002104230b0001020a000b010a031108010b000b020b03110001021401000006050b010b000f011502000200000001000300").unwrap(),
                hex::decode("a11ceb0b050000000c01001a021a3a03548a0304de032a0588048303078b07840e088f154006cf157410c316c3080a861f670ced1f87110df4301a0000010101020103010401050106010701080109010a010b000c000d0600000e0800000f0c00001007000011060000120600001306000c1504000939040203010001013c060005400401060102490800085a0b0000140001000016020300001704010000180401000019050300001a060700001b040800001c040800001d090100001e0a0300001f040400002006040000210b0c0000220b0d0000230e010000240403000025030f000026040800002710110000281213000029000100002a050300002b140300002c061500002d160100002e160100002f0401000030000100003100010000320501000648180400034a0001010007141a01000c4b1b03000721041c000c4c1d0100054d1f0101060c4e2003000c4f2003000c50210300045103030004520303000953232402030204540303000c552603000956282902030007570403000a580103000759040300075b040800085c012c00085d2d2e00015e2f04000c5f310300076004040004610303000b623401010001633536000364180101000765370100096601380203040c67013900096823010203000169183a0106096a3e24020300096b280802030007291a01000c6c1b0300096d3e410203000c6e390100016f2f1500072d160100072e16010007301a010007311a01001f19241e2a222d2738333a193c273e273c223f1e3f3b3f3c3f3d402741222d22243b44224427243c243d03060c0503000306080705030103010503070801050301060801050103030303010102070801050205030205050303030302010303060c030a0201080301070801010708070206080105020108030407080105030803010c02060c05060c0303050307080101060c01080b02060c030307080705030403030303020708070301080002070b0a0109000900020608070502060807030205080303070b08020900090109000901010709010903030303030301030301060807020803080702060b080209000901090001060901030501080305080c0303030301080c0106080c020303010608090f0303030303030303030306080701030608010803030608070303060b08020803080705050a020c0809010202070a09000a090002060c0a02020c080904060c030505010b080209000901010807010b0a01090001080401080501080602070b0802090009010900040c05080307080107050305010708070303010901020c0507030303030103070801040c0305070801090501010c030305060c08030f64656c65676174696f6e5f706f6f6c076163636f756e740a6170746f735f636f696e04636f696e056572726f72056576656e74067369676e6572057374616b650e7374616b696e675f636f6e666967057461626c650974696d657374616d7006766563746f7210706f6f6c5f7536345f756e626f756e640d4164645374616b654576656e740e44656c65676174696f6e506f6f6c1744656c65676174696f6e506f6f6c4f776e657273686970134f627365727665644c6f636b75704379636c6514526561637469766174655374616b654576656e7410556e6c6f636b5374616b654576656e741257697468647261775374616b654576656e74096164645f7374616b6504506f6f6c1a616d6f756e745f746f5f7368617265735f746f5f72656465656d1d6173736572745f64656c65676174696f6e5f706f6f6c5f657869737473176173736572745f6f776e65725f6361705f657869737473166275795f696e5f696e6163746976655f7368617265731a63616c63756c6174655f7374616b655f706f6f6c5f64726966741d63616e5f77697468647261775f70656e64696e675f696e6163746976651664656c65676174696f6e5f706f6f6c5f6578697374731a657865637574655f70656e64696e675f7769746864726177616c116765745f6164645f7374616b655f666565166765745f6f776e65645f706f6f6c5f61646472657373106765745f706f6f6c5f61646472657373096765745f7374616b65166861735f70656e64696e675f7769746864726177616c1a696e697469616c697a655f64656c65676174696f6e5f706f6f6c156f627365727665645f6c6f636b75705f6379636c650e6f6c635f776974685f696e646578106f776e65725f6361705f6578697374731c70656e64696e675f696e6163746976655f7368617265735f706f6f6c1970656e64696e675f7769746864726177616c5f65786973747310726561637469766174655f7374616b651472656465656d5f6163746976655f7368617265731672656465656d5f696e6163746976655f7368617265731a72657472696576655f7374616b655f706f6f6c5f7369676e6572137365745f64656c6567617465645f766f7465720c7365745f6f70657261746f721b73796e6368726f6e697a655f64656c65676174696f6e5f706f6f6c06756e6c6f636b0877697468647261771177697468647261775f696e7465726e616c0c706f6f6c5f616464726573731164656c656761746f725f616464726573730c616d6f756e745f61646465640d6164645f7374616b655f6665650d6163746976655f7368617265730f696e6163746976655f736861726573055461626c651370656e64696e675f7769746864726177616c73157374616b655f706f6f6c5f7369676e65725f636170105369676e65724361706162696c69747914746f74616c5f636f696e735f696e6163746976651e6f70657261746f725f636f6d6d697373696f6e5f70657263656e74616765106164645f7374616b655f6576656e74730b4576656e7448616e646c6517726561637469766174655f7374616b655f6576656e747313756e6c6f636b5f7374616b655f6576656e74731577697468647261775f7374616b655f6576656e747305696e64657806616d6f756e740f616d6f756e745f756e6c6f636b656410616d6f756e745f77697468647261776e0a616464726573735f6f66094170746f73436f696e087472616e73666572066275795f696e127570646174655f746f74616c5f636f696e730a656d69745f6576656e740762616c616e63650673686172657310616d6f756e745f746f5f73686172657310696e76616c69645f617267756d656e74096e6f745f666f756e6417626f72726f775f6d75745f776974685f64656661756c740d696e76616c69645f73746174650b746f74616c5f636f696e7306626f72726f77136765745f76616c696461746f725f73746174650b6e6f775f7365636f6e64730f6765745f6c6f636b75705f736563730d5374616b696e67436f6e6669671a69735f63757272656e745f65706f63685f76616c696461746f72036765740f6765745f7265776172645f726174651d6765745f7369676e65725f6361706162696c6974795f61646472657373217368617265735f746f5f616d6f756e745f776974685f746f74616c5f636f696e730c6765745f6f70657261746f720e616c72656164795f65786973747306617070656e64176372656174655f7265736f757263655f6163636f756e7408726567697374657216696e697469616c697a655f7374616b655f6f776e6572036e65770663726561746503616464106e65775f6576656e745f68616e646c650a626f72726f775f6d757408636f6e7461696e730d72656465656d5f7368617265730672656d6f76650d64657374726f795f656d7074791d6372656174655f7369676e65725f776974685f6361706162696c697479b7d68860645e7aff66895b2367686f75871601275245949da71a70befe6ffb1d0000000000000000000000000000000000000000000000000000000000000001030803000000000000000308050000000000000003080600000000000000030802000000000000000308010000000000000003080400000000000000030810270000000000000308ffffffffffffffff0a0221206170746f735f6672616d65776f726b3a3a64656c65676174696f6e5f706f6f6c126170746f733a3a6d657461646174615f7631ae0806010000000000000014454f574e45525f4341505f4e4f545f464f554e444844656c65676174696f6e20706f6f6c206f776e6572206361706162696c69747920646f6573206e6f74206578697374206174207468652070726f7669646564206163636f756e742e020000000000000019454f574e45525f4341505f414c52454144595f4558495354532c4163636f756e7420697320616c7265616479206f776e696e6720612064656c65676174696f6e20706f6f6c2e03000000000000001f4544454c45474154494f4e5f504f4f4c5f444f45535f4e4f545f45584953543c44656c65676174696f6e20706f6f6c20646f6573206e6f74206578697374206174207468652070726f766964656420706f6f6c20616464726573732e04000000000000001a4550454e44494e475f5749544844524157414c5f45584953545347546865726520697320612070656e64696e67207769746864726177616c20746f206265206578656375746564206265666f726520756e6c6f636b696e6720616e79207374616b6505000000000000001e45494e56414c49445f434f4d4d495353494f4e5f50455243454e544147453f436f6d6d697373696f6e2070657263656e746167652068617320746f206265206265747765656e203020616e6420604d41585f46454560202d20313030252e06000000000000002345534c41534845445f494e4143544956455f5354414b455f4f4e5f504153545f4f4c43d302536c617368696e672028696620696d706c656d656e746564292073686f756c64206e6f74206265206170706c69656420746f20616c72656164792060696e61637469766560207374616b652e0a204e6f74206f6e6c7920697420696e76616c69646174657320746865206163636f756e74696e67206f662070617374206f62736572766564206c6f636b7570206379636c657320284f4c43292c0a2062757420697320616c736f20756e6661697220746f2064656c656761746f72732077686f7365207374616b6520686173206265656e20696e616374697665206265666f72652076616c696461746f722073746172746564206d69736265686176696e672e0a204164646974696f6e616c6c792c2074686520696e616374697665207374616b6520646f6573206e6f7420636f756e74206f6e2074686520766f74696e6720706f776572206f662076616c696461746f722e0008096765745f7374616b65010000106f776e65725f6361705f657869737473010000116765745f6164645f7374616b655f666565010000156f627365727665645f6c6f636b75705f6379636c650100001664656c65676174696f6e5f706f6f6c5f657869737473010000166765745f6f776e65645f706f6f6c5f61646472657373010000166861735f70656e64696e675f7769746864726177616c0100001d63616e5f77697468647261775f70656e64696e675f696e616374697665010000000204330534053503360301020b370807240803380b0802080308073a0b08020508033b08093d033e033f0b0a010800410b0a010804420b0a010805430b0a010806020201330503020144030402033305340545030502033305340546030602033305340547030001040101173b0a01111a0a020600000000000000002104090b0001020a012a010c080a00111e0c060a010a0211090c050b000a010a0238000a082e11170c030e030a0211200a080f000a060a020a05171121010a011122010c07010c040a080f000b040b071611230b080f010b010b060b020b0512003801020100000003110a020a000a01112526040b0b000b0111260c03050f0b000b0211270c030b03020200000001080b001107040405070700112827020300000001080b00111104040507070411292702040000000f240a020600000000000000002104080b0001060000000000000000020a000a0111080a001002140c030a000f030a010a033802140b03210419051e0b00010705112b270b0011120b010b0211210205000000255f0a00110b11220c090c080c060c030a060a0010041426040e05130b00010702112b270a060a00100414240c070b030b08160c030a0704250b060a00100414170c090a001000112c0c040a030a042404380a030b04170a001005141807061a0c01053a0600000000000000000c010b010c040a0010060a001002143803112c0c050a090a052404530a090b05170b001005141807061a0c0205570b00010600000000000000000c020b020c050b070b030b090b040b050206010000080f0a00112e070521040b112f0b001130260c01050d090c010b01020701000001030b00290102080000002a1e0a000a010c022e0b0211130c0404120e041007140a001002100714230c030514090c030b03041b0b000b010707111d051d0b000102090100002b210b001131041d11320c020e0211330c060c050a060600000000000000002404180b01350b0535180b06351a340c03051a0600000000000000000c030b030c04051f0600000000000000000c040b04020a0100010201070a0011030b002b02100814020b00000001040b0010091134020c0100010130700a0011020a002b010c0f0a0f11050c0a0c090c0e0c080c0d0a0f10000a0f10000a0111260b080a091711350c080a0f0a0111130c10044e0a0f10060a1038030c0c0e101007140b0f10021007142304310b0c0a0111250600000000000000000c050c0405490a0c0b0c0a0111260b0e0a0a1711350c0e0a0d04410b0e0600000000000000000c030c0205450600000000000000000b0e0c030c020b020b030c050c040b040b050c070c0605540b0f010600000000000000000600000000000000000c070c060b060b070c0e0c0b0b010b00113621046c0b080b09160c080b0d04680b0b0b0a160c0b056c0b0e0b0a160c0e0b080b0b0b0e020d010001010f0b0a0011020b002b010b0111130c020e02100714020e010400324d0a00111e0c040a041111200408050d0b000107031137270a01070625041205170b00010701112827403300000000000000000c060d06070838040d060b0238040a000b0611390c080c070e0738050e07111e0c050e070600000000000000000a040b04113b38060c030d030600000000000000001110113d38070e07113d06000000000000000011100b0338080b080600000000000000000b010e0738090e07380a0e07380b0e07380c12012d010b000b0512022d02020f0100010101080a0011020b002b011002100714021000000001030b001203021101000001030b00290202120000000f090a001002140c010b000f060b01380d021300000013180a0010030a01380e040e080b0010030b01380f140c030c0205150b00010906000000000000000011100c030c020b020b030214010401013f2e0a01111a0a020600000000000000002104090b0001020a012a010c060b00111e0c040a061002140c050a060a040b020b0511160c020a062e11170c030e030a0211420a060f000a040a021121010b060f0a0b010b040b0212043810021500000003140a0010000a010b0211010c030a0306000000000000000021040e0b0001060000000000000000020b000f000b010b0311430216000000404c0a000f060a03380d0c080a080a010b020c050c042e0b040b0511010c0a0a0a0600000000000000002104190b00010b0801060000000000000000020a080a010b0a11430c090a080a010c062e0b06112606000000000000000021042c0a000f030b013811010e031007140a00100210071423043c0b082e112c060000000000000000210c0705400b0801090c070b0704480b000f060b0338121145054a0b00010b09021700000001040b001009114602180104020102420e0b00111e110a0c030a03111a0b032b0111170c020e020b01114702190104020102420e0b00111e110a0c030a03111a0b032b0111170c020e020b011148021a0104010143470a0011020a002a010c070a072e11050c030c020c060c010c050a070f000b010a021711230a0711120b060a031711230a070f000a0011360b021121010a070a0011360b031104010b0504440b00112201010c04010b040a070f04150a071002100714060100000000000000160a070f020f07150a070f060b07100214113d380705460b0701021b0104010144380a0111220101010c040a020b0425040b05100b000107021128270a01111a0a020600000000000000002104190b0001020a012a010c060b00111e0c050a060a050b0211150c020a062e11170c030e030a0211490a060a050a021104010b060f0b0b010b050b0212053813021c0104010101090a01111a0b012a010b00111e0b02111d021d00000045720a020600000000000000002104070b0001020a002e110b0c090a000a010c032e0b0311130c0b04250e0b1007140a00100210071423041f080c0405220a0911060c040b040c050527090c050b0520042d0b0001020a000a010b020a0b11160c020a002e11170c060e060c0a0a09110604590a0911220c080101010e0b1007140a00100210071421044f0b080a02170c080a0a0a0811420a0a0a02114a0a0a0b081149055c0a0a0a02114a0b0a0a010a0238000a09112201010c07010b070a000f04150b000f0c0b090b010b021206381402010001070101010301050106010203000200010401080109010a00").unwrap(),
            ],
        ),
    );

    let results = h.run_block(vec![publish_tx]);
    assert_success!(results[0]);
}

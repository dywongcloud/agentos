use std::ptr;

use holoiroh_ios_bridge::{holoiroh_ios_bridge_free, holoiroh_ios_bridge_new_with_secret_key};
use iroh::endpoint::presets;
use iroh::{Endpoint, SecretKey};

fn main() {
    let seed = [0x5au8; 32];

    let null_key = unsafe { holoiroh_ios_bridge_new_with_secret_key(ptr::null(), seed.len()) };
    assert!(null_key.is_null());
    for wrong_len in [0, 1, 31, 33, 64] {
        let bridge = unsafe { holoiroh_ios_bridge_new_with_secret_key(seed.as_ptr(), wrong_len) };
        assert!(bridge.is_null(), "key length {wrong_len} must be rejected");
    }

    let expected_id = SecretKey::from_bytes(&seed).public();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("probe runtime must build");
    let (first_id, second_id) = runtime.block_on(async {
        let first = Endpoint::builder(presets::N0)
            .secret_key(SecretKey::from_bytes(&seed))
            .bind()
            .await
            .expect("first keyed endpoint must bind");
        let first_id = first.id();
        assert_eq!(first_id, expected_id);
        first.close().await;

        let second = Endpoint::builder(presets::N0)
            .secret_key(SecretKey::from_bytes(&seed))
            .bind()
            .await
            .expect("second keyed endpoint must bind after first closes");
        let second_id = second.id();
        assert_eq!(second_id, expected_id);
        second.close().await;
        (first_id, second_id)
    });
    drop(runtime);
    assert_eq!(first_id, second_id);

    for construction in 1..=2 {
        let bridge = unsafe { holoiroh_ios_bridge_new_with_secret_key(seed.as_ptr(), seed.len()) };
        assert!(
            !bridge.is_null(),
            "keyed FFI construction {construction} failed"
        );
        unsafe { holoiroh_ios_bridge_free(bridge) };
    }

    println!(
        "persistent_identity_probe: OK node_id={first_id} sequential_endpoints=2 ffi_null_rejected=true ffi_wrong_lengths_rejected=5 ffi_keyed_bridges=2"
    );
}

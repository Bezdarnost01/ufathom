//! Control for `global_pool.rs`: using rayon's global pool makes
//! `build_global` fail, so the probe in that file can fail.

#[test]
fn using_the_global_pool_makes_the_probe_fail() {
    let (a, b) = rayon::join(|| 1, || 2);
    assert_eq!(a + b, 3);
    assert!(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build_global()
            .is_err()
    );
}

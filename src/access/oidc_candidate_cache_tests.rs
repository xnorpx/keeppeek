use super::*;

#[test]
fn candidate_cache_keys_resolved_policy_and_shares_refresh_state() {
    let fixture = FixtureProvider::new();
    let mut cache = CandidateCache::default();
    let now = Instant::now();
    let first = cache
        .get(&fixture.config, now, || {
            Provider::discover(&fixture.config, fixture.transport.clone())
        })
        .unwrap();
    first.budget.lock().unwrap().reserve_emergency(now).unwrap();
    let repeated = cache
        .get(&fixture.config, now, || panic!("must reuse the provider"))
        .unwrap();
    assert!(Arc::ptr_eq(&first.keys, &repeated.keys));
    assert_eq!(repeated.budget.lock().unwrap().last_emergency, Some(now));
    let mut changed = fixture.config.clone();
    changed.client_id = "different-client".into();
    assert!(
        cache
            .get(&changed, now, || panic!(
                "same issuer policy variant must not reset its budget"
            ))
            .is_err()
    );
    assert!(
        cache
            .get(&changed, now, || panic!(
                "failed discovery cooldown must persist"
            ))
            .is_err()
    );
    changed.issuer = "https://different.example".into();
    assert!(
        cache
            .get(&changed, now, || Err(anyhow!(
                "different issuer must discover"
            )))
            .is_err()
    );
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn candidate_cache_never_evicts_live_handles_or_resets_emergency_cooldown() {
    let fixture = FixtureProvider::new();
    let mut cache = CandidateCache::default();
    let now = Instant::now();
    let provider = cache
        .get(&fixture.config, now, || {
            Provider::discover(&fixture.config, fixture.transport.clone())
        })
        .unwrap();
    let later = now + Duration::from_secs(301);
    assert!(
        cache
            .get(&fixture.config, later, || panic!(
                "in-flight handle must pin provider"
            ))
            .is_err()
    );
    provider
        .budget
        .lock()
        .unwrap()
        .reserve_emergency(later)
        .unwrap();
    drop(provider);
    assert!(
        cache
            .get(&fixture.config, later, || panic!(
                "JWKS cooldown must pin provider"
            ))
            .is_err()
    );
    assert!(
        cache
            .get(&fixture.config, later + Duration::from_secs(61), || Err(
                anyhow!("expired metadata must rediscover")
            ))
            .is_err()
    );
}

#[test]
fn candidate_cache_capacity_rejects_without_evicting_and_recovers_after_expiry() {
    let fixture = FixtureProvider::new();
    let mut cache = CandidateCache::default();
    let now = Instant::now();
    for index in 0..4 {
        let mut settings = fixture.config.clone();
        settings.client_id = format!("candidate-{index}");
        settings.issuer = format!("https://candidate-{index}.example");
        assert!(
            cache
                .get(&settings, now, || Err(anyhow!("offline")))
                .is_err()
        );
    }
    assert_eq!(cache.entries.len(), 4);
    assert!(
        cache
            .get(&fixture.config, now, || panic!(
                "capacity must reject before discovery"
            ))
            .is_err()
    );
    assert_eq!(cache.entries.len(), 4);
    let provider = cache
        .get(&fixture.config, now + Duration::from_secs(301), || {
            Provider::discover(&fixture.config, fixture.transport.clone())
        })
        .unwrap();
    assert_eq!(cache.entries.len(), 1);
    assert!(provider.config == fixture.config);
}

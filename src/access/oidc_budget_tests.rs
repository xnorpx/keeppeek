use super::*;

#[test]
fn issuer_budget_survives_failed_discovery_cache_revisions_and_candidate_policy_changes() {
    let settings: Oidc = toml::from_str(
        "issuer = 'https://issuer.example'\nclient_id = 'active'\nredirect_uri = 'https://keeppeek.example/auth/callback'",
    ).unwrap();
    let budgets = IssuerBudgets::default();
    let mut active = Cache::default();
    let mut candidate = CandidateCache::default();
    let now = Instant::now();
    let attempts = std::cell::Cell::new(0);
    let discover = || -> Result<Provider> {
        let _lease = budgets.reserve_discovery(&settings.issuer, now)?;
        attempts.set(attempts.get() + 1);
        Err(anyhow!("synthetic failed discovery"))
    };
    assert!(active.get(1, "active", now, discover).is_err());
    let mut changed = settings.clone();
    changed.client_id = "candidate-client".into();
    assert!(candidate.get(&changed, now, discover).is_err());
    assert!(active.get(2, "renamed", now, discover).is_err());
    assert_eq!(attempts.get(), 1);
    let boundary = now + Duration::from_secs(300);
    assert!(
        active
            .get(3, "active", boundary, || {
                let _lease = budgets.reserve_discovery(&settings.issuer, boundary)?;
                attempts.set(attempts.get() + 1);
                Err(anyhow!("synthetic failed discovery"))
            })
            .is_err()
    );
    assert_eq!(attempts.get(), 2);
}

#[test]
fn issuer_budget_capacity_preserves_leases_and_cooldowns() {
    let budgets = IssuerBudgets::default();
    let now = Instant::now();
    let leases: Vec<_> = (0..8)
        .map(|index| {
            budgets
                .reserve_discovery(&format!("https://issuer-{index}.example"), now)
                .unwrap()
        })
        .collect();
    let ninth = "https://ninth.example";
    assert!(budgets.reserve_discovery(ninth, now).is_err());
    let later = now + Duration::from_secs(300);
    assert!(budgets.reserve_discovery(ninth, later).is_err());
    leases[0].lock().unwrap().reserve_emergency(later).unwrap();
    drop(leases);
    let replacement = budgets.reserve_discovery(ninth, later).unwrap();
    assert_eq!(budgets.entries.lock().unwrap().len(), 2);
    assert!(
        budgets
            .entries
            .lock()
            .unwrap()
            .contains_key("https://issuer-0.example/")
    );
    drop(replacement);
    let _lease = budgets
        .reserve_discovery("https://next.example", later + Duration::from_secs(60))
        .unwrap();
    assert_eq!(budgets.entries.lock().unwrap().len(), 2);
}

#[test]
fn issuer_budget_does_not_evict_unleased_failed_attempts_before_the_boundary() {
    let budgets = IssuerBudgets::default();
    let now = Instant::now();
    for index in 0..8 {
        budgets
            .reserve_discovery(&format!("https://issuer-{index}.example"), now)
            .unwrap();
    }
    assert!(
        budgets
            .reserve_discovery("https://ninth.example", now + Duration::from_secs(299))
            .is_err()
    );
    assert!(
        budgets
            .reserve_discovery("https://ninth.example", now + Duration::from_secs(300))
            .is_ok()
    );
    assert_eq!(budgets.entries.lock().unwrap().len(), 1);
}

#[test]
fn issuer_budget_reserves_emergency_attempts_once_across_provider_generations() {
    let budgets = IssuerBudgets::default();
    let now = Instant::now();
    let first = budgets
        .reserve_discovery("https://issuer.example", now)
        .unwrap();
    let later = now + Duration::from_secs(300);
    let next = budgets
        .reserve_discovery("https://issuer.example/", later)
        .unwrap();
    assert!(Arc::ptr_eq(&first, &next));
    first.lock().unwrap().reserve_emergency(later).unwrap();
    assert!(next.lock().unwrap().reserve_emergency(later).is_err());
    assert!(
        next.lock()
            .unwrap()
            .reserve_emergency(later + Duration::from_secs(59))
            .is_err()
    );
    next.lock()
        .unwrap()
        .reserve_emergency(later + Duration::from_secs(60))
        .unwrap();
}

#[test]
fn issuer_budget_concurrent_discovery_has_one_winner() {
    let budgets = IssuerBudgets::default();
    let barrier = std::sync::Barrier::new(3);
    let now = Instant::now();
    std::thread::scope(|scope| {
        let attempt = || {
            barrier.wait();
            budgets
                .reserve_discovery("https://issuer.example", now)
                .is_ok()
        };
        let first = scope.spawn(attempt);
        let second = scope.spawn(attempt);
        barrier.wait();
        assert_ne!(first.join().unwrap(), second.join().unwrap());
    });
}

#[test]
fn issuer_budget_keeps_real_discovery_failure_cooldown() {
    let fixture = FixtureProvider::new();
    let budgets = IssuerBudgets::default();
    let now = Instant::now();
    fixture.set_outage();
    let error =
        Provider::discover_with_budget(&fixture.config, fixture.transport.clone(), &budgets, now)
            .err()
            .unwrap();
    assert!(error.downcast_ref::<super::ProviderUnavailable>().is_some());
    let error =
        Provider::discover_with_budget(&fixture.config, fixture.transport.clone(), &budgets, now)
            .err()
            .unwrap();
    assert_eq!(error.to_string(), "OIDC discovery is temporarily limited");
    assert!(budgets.entries.try_lock().is_ok());
}

#[test]
fn issuer_budget_is_shared_by_real_providers_and_failed_jwks_refreshes() {
    let fixture = FixtureProvider::new();
    let budgets = IssuerBudgets::default();
    let now = Instant::now();
    let first =
        Provider::discover_with_budget(&fixture.config, fixture.transport.clone(), &budgets, now)
            .unwrap();
    let mut changed = fixture.config.clone();
    changed.client_id = "different-client".into();
    assert!(
        Provider::discover_with_budget(&changed, fixture.transport.clone(), &budgets, now).is_err()
    );
    let second = Provider::discover_with_budget(
        &changed,
        fixture.transport.clone(),
        &budgets,
        now + Duration::from_secs(300),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&first.budget, &second.budget));
    assert!(!Arc::ptr_eq(&first.keys, &second.keys));
    let (token, _) = tests::sign_generation(tests::claims(), 1);
    fixture.set_outage();
    let error = first.keys_for(&token).err().unwrap();
    assert!(error.downcast_ref::<ProviderUnavailable>().is_some());
    let error = second.keys_for(&token).err().unwrap();
    assert!(error.downcast_ref::<ProviderUnavailable>().is_none());
    assert_eq!(error.to_string(), "OIDC key refresh is temporarily limited");
    assert!(budgets.entries.try_lock().is_ok());
    assert!(first.budget.try_lock().is_ok());
}

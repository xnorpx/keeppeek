use super::*;

fn handoff(f: &mut Fixture, token: &str) -> (Uuid, String) {
    let session = f.store.exchange(token, &f.context, f.now).unwrap();
    (session.id, session.handle.into_string())
}

#[test]
fn status_receipt_binds_handle_public_id_and_context_without_consuming_media() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let (id, handle) = handoff(&mut f, &token);
    let discovered = f
        .store
        .inspect_session(&handle, None, &f.context, f.now)
        .unwrap();
    assert_eq!(discovered.session_id, id);
    assert_eq!(discovered.grant, grant);
    assert!(discovered.media_active);
    assert!(!discovered.stream_committed);
    let inspected = f
        .store
        .inspect_session(&handle, Some(id), &f.context, f.now)
        .unwrap();
    assert_eq!(inspected, discovered);
    assert_eq!(inspected.grant.views, 0);
    assert_eq!(inspected.grant.downloads, 0);
    assert!(!format!("{inspected:?}").contains(&handle));
    assert!(!format!("{inspected:?}").contains(&token));
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    f.commit(&ticket).unwrap();
    let last_view = f
        .store
        .inspect_session(&handle, Some(id), &f.context, f.now)
        .unwrap();
    assert!(last_view.stream_committed);
    assert!(last_view.media_deadline.is_some());
    assert_eq!(
        f.store
            .inspect_session(&handle, Some(id), &f.context, f.now)
            .unwrap()
            .grant
            .views,
        1
    );
}

#[test]
fn status_and_end_reject_cross_tab_ids_and_changed_context_without_retiring_owner() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let (first_id, first) = handoff(&mut f, &token);
    let (second_id, second) = handoff(&mut f, &token);
    assert_eq!(
        f.store
            .inspect_session(&first, Some(second_id), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
    assert_eq!(
        f.store
            .inspect_session(&second, Some(first_id), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
    assert_eq!(
        f.store
            .end_session(&first, second_id, &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
    let changed = SessionContext {
        policy_revision: 2,
        ..f.context.clone()
    };
    assert_eq!(
        f.store
            .inspect_session(&first, Some(first_id), &changed, f.now)
            .unwrap_err(),
        Error::Denied
    );
    let changed = SessionContext {
        origin: "https://other.example".into(),
        ..f.context.clone()
    };
    assert_eq!(
        f.store
            .end_session(&first, first_id, &changed, f.now)
            .unwrap_err(),
        Error::Denied
    );
    assert!(
        f.store
            .inspect_session(&first, Some(first_id), &f.context, f.now)
            .unwrap()
            .media_active
    );
    assert!(
        f.store
            .inspect_session(&second, Some(second_id), &f.context, f.now)
            .unwrap()
            .media_active
    );
}

#[test]
fn revoked_receipt_reports_only_its_bound_grant_and_never_reauthorizes_media() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let (id, handle) = handoff(&mut f, &token);
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    f.commit(&ticket).unwrap();
    let revoked = f
        .store
        .revoke(grant.id, grant.revision, "withdrawn", f.now)
        .unwrap();
    let bound = f
        .store
        .inspect_session(&handle, Some(id), &f.context, f.now)
        .unwrap();
    assert_eq!(bound.grant, revoked);
    assert_eq!(bound.grant.views, 1);
    assert!(!bound.media_active);
    assert_eq!(f.begin(&handle, Mode::Stream).unwrap_err(), Error::Denied);
    assert_eq!(f.authorize(&ticket, f.now).unwrap_err(), Error::Denied);
    assert_eq!(
        f.store.exchange(&token, &f.context, f.now).unwrap_err(),
        Error::Denied
    );
    f.store.end_session(&handle, id, &f.context, f.now).unwrap();
    assert_eq!(
        f.store
            .inspect_session(&handle, Some(id), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
}

#[test]
fn expired_grant_keeps_fixed_status_receipt_until_five_minutes_without_poll_refresh() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let (id, handle) = handoff(&mut f, &token);
    f.now.unix_ms += 60_000;
    f.now.monotonic += Duration::from_secs(60);
    let bound = f
        .store
        .inspect_session(&handle, Some(id), &f.context, f.now)
        .unwrap();
    assert_eq!(bound.grant.id, grant.id);
    assert_eq!(bound.grant.revoked_at_ms, None);
    assert!(!bound.media_active);
    assert_eq!(f.begin(&handle, Mode::Stream).unwrap_err(), Error::Denied);
    f.now.unix_ms += 239_999;
    f.now.monotonic += Duration::from_millis(239_999);
    assert!(
        !f.store
            .inspect_session(&handle, Some(id), &f.context, f.now)
            .unwrap()
            .media_active
    );
    f.now.unix_ms += 1;
    f.now.monotonic += Duration::from_millis(1);
    assert_eq!(
        f.store
            .inspect_session(&handle, Some(id), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
}

#[test]
fn status_receipt_monotonic_deadline_does_not_require_wall_clock_progress() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let (id, handle) = handoff(&mut f, &token);
    f.now.monotonic += Duration::from_secs(61);
    assert!(
        !f.store
            .inspect_session(&handle, Some(id), &f.context, f.now)
            .unwrap()
            .media_active
    );
    f.now.monotonic += Duration::from_secs(239);
    assert_eq!(
        f.store
            .inspect_session(&handle, Some(id), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
}

#[test]
fn receipt_overflow_evicts_oldest_terminal_receipt_without_displacing_live_media() {
    let mut f = Fixture::new();
    let (_, live_token) = f.grant();
    let (live_id, live_handle) = handoff(&mut f, &live_token);
    let mut terminal = Vec::new();
    for _ in 0..16 {
        f.now.unix_ms += 1;
        f.now.monotonic += Duration::from_millis(1);
        f.intent.request_id = Uuid::new_v4();
        let (grant, token) = f.grant();
        let (id, handle) = handoff(&mut f, &token);
        f.store
            .revoke(grant.id, grant.revision, "withdrawn", f.now)
            .unwrap();
        terminal.push((id, handle));
    }
    assert!(
        f.store
            .inspect_session(&live_handle, Some(live_id), &f.context, f.now)
            .unwrap()
            .media_active
    );
    let (id, handle) = &terminal[0];
    assert_eq!(
        f.store
            .inspect_session(handle, Some(*id), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
    for (id, handle) in terminal.iter().skip(1) {
        assert!(
            !f.store
                .inspect_session(handle, Some(*id), &f.context, f.now)
                .unwrap()
                .media_active
        );
    }
}

#[test]
fn end_session_removes_receipt_pending_tickets_and_media_without_refunding_consumption() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let (id, handle) = handoff(&mut f, &token);
    let committed = f.begin(&handle, Mode::Stream).unwrap();
    f.commit(&committed).unwrap();
    let pending = f.begin(&handle, Mode::Stream).unwrap();
    f.store.end_session(&handle, id, &f.context, f.now).unwrap();
    assert_eq!(f.authorize(&committed, f.now).unwrap_err(), Error::Denied);
    assert_eq!(f.commit(&pending).unwrap_err(), Error::Denied);
    assert_eq!(
        f.store
            .inspect_session(&handle, None, &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap()[0].views, 1);
    let next = f.store.exchange(&token, &f.context, f.now).unwrap();
    assert_ne!(next.id, id);
}

#[test]
fn restart_invalidates_both_live_and_terminal_receipts() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let (first_id, first) = handoff(&mut f, &token);
    f.store
        .revoke(grant.id, grant.revision, "withdrawn", f.now)
        .unwrap();
    f.intent.request_id = Uuid::new_v4();
    let (_, token) = f.grant();
    let (second_id, second) = handoff(&mut f, &token);
    let path = f.directory.0.join("grants.db");
    drop(f.store);
    f.store = Store::open(&path, f.now.unix_ms).unwrap();
    assert_eq!(
        f.store
            .inspect_session(&first, Some(first_id), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
    assert_eq!(
        f.store
            .inspect_session(&second, Some(second_id), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
}

#[test]
fn failed_revocation_preserves_media_and_receipt_until_durable_success() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let (id, handle) = handoff(&mut f, &token);
    f.store.fail_next_commit();
    assert_eq!(
        f.store
            .revoke(grant.id, grant.revision, "withdrawn", f.now)
            .unwrap_err(),
        Error::Storage
    );
    let bound = f
        .store
        .inspect_session(&handle, Some(id), &f.context, f.now)
        .unwrap();
    assert_eq!(bound.grant, grant);
    assert!(bound.media_active);
    f.store
        .revoke(grant.id, grant.revision, "withdrawn", f.now)
        .unwrap();
    assert!(
        !f.store
            .inspect_session(&handle, Some(id), &f.context, f.now)
            .unwrap()
            .media_active
    );
}

#[test]
fn management_snapshot_and_batch_revoke_are_bounded_atomic_and_revision_checked() {
    let mut f = Fixture::new();
    let (first, _) = f.grant();
    f.intent.request_id = Uuid::new_v4();
    let (second, _) = f.grant();
    assert_eq!(f.store.get(first.id, f.now).unwrap(), Some(first.clone()));
    assert_eq!(f.store.get(Uuid::new_v4(), f.now).unwrap(), None);
    assert_eq!(f.store.all(f.now).unwrap().len(), 2);
    let bad = [(first.id, first.revision), (second.id, second.revision + 1)];
    assert_eq!(
        f.store.revoke_batch(&bad, "withdrawn", f.now).unwrap_err(),
        Error::Conflict
    );
    assert!(
        f.store
            .all(f.now)
            .unwrap()
            .iter()
            .all(|grant| grant.revoked_at_ms.is_none())
    );
    f.store.fail_next_commit();
    let good = [(first.id, first.revision), (second.id, second.revision)];
    assert_eq!(
        f.store.revoke_batch(&good, "withdrawn", f.now).unwrap_err(),
        Error::Storage
    );
    assert!(
        f.store
            .all(f.now)
            .unwrap()
            .iter()
            .all(|grant| grant.revoked_at_ms.is_none())
    );
    let revoked = f.store.revoke_batch(&good, "withdrawn", f.now).unwrap();
    assert_eq!(revoked.len(), 2);
    assert!(
        revoked
            .iter()
            .all(|grant| grant.revoked_at_ms == Some(f.now.unix_ms))
    );
    assert_eq!(
        f.store
            .revoke_batch(&[(Uuid::new_v4(), 1); 257], "withdrawn", f.now)
            .unwrap_err(),
        Error::Invalid
    );
}

#[test]
fn effective_clock_expires_fresh_handoffs_when_wall_freezes_and_persists_observation() {
    let mut f = Fixture::new();
    let mut anchor = ClockAnchor::new(f.now).unwrap();
    let (_, token) = f.grant();
    let mut raw = f.now;
    raw.monotonic += Duration::from_secs(61);
    let effective = anchor.sample(raw).unwrap();
    assert_eq!(effective.unix_ms, f.now.unix_ms + 61_000);
    assert_eq!(
        f.store.exchange(&token, &f.context, effective).unwrap_err(),
        Error::Denied
    );
    let path = f.directory.0.join("grants.db");
    drop(f.store);
    assert_eq!(Store::open(&path, raw.unix_ms).unwrap_err(), Error::Denied);
}

#[test]
fn clock_anchor_preserves_fractional_elapsed_time_and_freeze_after_forward_jump() {
    let f = Fixture::new();
    let mut raw = f.now;
    let mut anchor = ClockAnchor::new(raw).unwrap();
    let mut effective = raw;
    for _ in 0..20 {
        raw.monotonic += Duration::from_micros(100);
        effective = anchor.sample(raw).unwrap();
    }
    assert_eq!(effective.unix_ms, raw.unix_ms + 2);
    raw.unix_ms += 60_000;
    raw.monotonic += Duration::from_millis(1);
    assert_eq!(anchor.sample(raw).unwrap().unix_ms, raw.unix_ms);
    raw.monotonic += Duration::from_secs(60);
    assert_eq!(anchor.sample(raw).unwrap().unix_ms, raw.unix_ms + 60_000);
}

#[test]
fn clock_anchor_rejects_raw_rollback_and_arithmetic_overflow_without_rewinding() {
    let f = Fixture::new();
    let mut raw = f.now;
    let mut anchor = ClockAnchor::new(raw).unwrap();
    raw.unix_ms += 1;
    raw.monotonic += Duration::from_millis(1);
    anchor.sample(raw).unwrap();
    let earlier_wall = Clock {
        unix_ms: raw.unix_ms - 1,
        ..raw
    };
    assert_eq!(anchor.sample(earlier_wall).unwrap_err(), Error::Denied);
    let earlier_monotonic = Clock {
        monotonic: f.now.monotonic,
        ..raw
    };
    assert_eq!(anchor.sample(earlier_monotonic).unwrap_err(), Error::Denied);
    raw.monotonic += Duration::from_millis(1);
    assert_eq!(anchor.sample(raw).unwrap().unix_ms, f.now.unix_ms + 2);
    let ceiling = Clock {
        unix_ms: i64::MAX - 1,
        ..raw
    };
    let mut overflow = ClockAnchor::new(ceiling).unwrap();
    let too_late = Clock {
        monotonic: raw.monotonic + Duration::from_millis(2),
        ..ceiling
    };
    assert_eq!(overflow.sample(too_late).unwrap_err(), Error::Denied);
    assert_eq!(
        ClockAnchor::new(Clock { unix_ms: -1, ..raw }).unwrap_err(),
        Error::Invalid
    );
}

#[test]
fn policy_invalidation_frees_all_ephemeral_capacity_without_refunding_durable_views() {
    let mut f = Fixture::new();
    f.intent.max_views = Some(2);
    let mut sessions = Vec::new();
    let mut tickets = Vec::new();
    for _ in 0..4 {
        f.intent.request_id = Uuid::new_v4();
        let (_, token) = f.grant();
        for index in 0..2 {
            let (id, handle) = handoff(&mut f, &token);
            let ticket = f.begin(&handle, Mode::Stream).unwrap();
            if index == 0 {
                f.commit(&ticket).unwrap();
            }
            tickets.push(ticket);
            sessions.push((id, handle));
        }
    }
    f.intent.request_id = Uuid::new_v4();
    let (_, waiting) = f.grant();
    assert_eq!(
        f.store.exchange(&waiting, &f.context, f.now).unwrap_err(),
        Error::Capacity
    );
    let before = f.store.all(f.now).unwrap();
    assert_eq!(before.iter().map(|grant| grant.views).sum::<u32>(), 4);
    f.store.invalidate_sessions();
    assert_eq!(f.store.all(f.now).unwrap(), before);
    for (id, handle) in sessions {
        assert_eq!(
            f.store
                .inspect_session(&handle, Some(id), &f.context, f.now)
                .unwrap_err(),
            Error::Denied
        );
    }
    for ticket in tickets {
        assert_eq!(f.authorize(&ticket, f.now).unwrap_err(), Error::Denied);
    }
    assert!(f.store.exchange(&waiting, &f.context, f.now).is_ok());
}

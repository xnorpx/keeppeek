use super::*;
use crate::server::{self, guest_review::registry::tests::Harness, proto};

struct Transfer {
    reader: Box<dyn Read + Send>,
    digest: Sha256,
    count: usize,
    grant_id: Uuid,
}

fn record_administrator_download(
    fixture: &Evidence,
    expected: &ExportBinding,
    system: &mut sysinfo::System,
) -> (Duration, u64) {
    let principal = server::ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let start = Instant::now();
    let (_, messages) = server::download_export(
        fixture.state(),
        &principal.id(),
        proto::DownloadExport {
            job_id: expected.job_id.clone(),
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    )
    .unwrap();
    let elapsed = start.elapsed();
    let retained_rss = resident_bytes(system);
    let mut hash = Sha256::new();
    for message in messages {
        let Some(proto::message::Message::Export(export)) = message.message.message else {
            panic!("expected an export")
        };
        let Some(proto::export_message::Message::FileChunk(chunk)) = export.message else {
            panic!("expected an export chunk")
        };
        hash.update(chunk.payload);
    }
    assert_eq!(<[u8; 32]>::from(hash.finalize()), expected.sha256);
    (elapsed, retained_rss)
}

fn open_download(
    fixture: &Evidence,
    harness: &mut Harness,
    bound: &BoundSession,
    session: &str,
) -> (Transfer, Duration) {
    let request = session_request(Mode::Download, None, bound.session_id, session);
    let guard = fixture
        .state()
        .guest_review
        .admit_request(
            "127.0.0.1".parse().unwrap(),
            super::super::super::limits::Action::Download,
            harness.request(),
        )
        .unwrap();
    let start = Instant::now();
    let response = prepare(
        &request,
        fixture.state(),
        &fixture.context,
        guard,
        Mode::Download,
    )
    .unwrap();
    let (mut reader, size) = response.data.into_reader_and_size();
    assert_eq!(
        size,
        Some(usize::try_from(bound.grant.intent.binding.bytes).unwrap())
    );
    let mut first = [0u8; 1024];
    let count = reader.read(&mut first).unwrap();
    let elapsed = start.elapsed();
    assert!(count > 0);
    let mut digest = Sha256::new();
    digest.update(&first[..count]);
    (
        Transfer {
            reader,
            digest,
            count,
            grant_id: bound.grant.id,
        },
        elapsed,
    )
}

fn revoke_and_observe(fixture: &Evidence, mut transfer: Transfer) -> Duration {
    let principal = server::ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let start = Instant::now();
    sharing::dispatch(
        fixture.state(),
        &principal,
        proto::ShareCommand {
            action: Some(proto::share_command::Action::Revoke(
                proto::RevokeShareGrant {
                    grant_id: transfer.grant_id.to_string(),
                    expected_revision: 1,
                    reason: "Review complete".to_owned(),
                },
            )),
        },
    )
    .unwrap();
    // The recipient's next normal media read occurs at its 20 ms playback cadence.
    std::thread::sleep(Duration::from_millis(20));
    let mut output = [0x5b; 1024];
    assert!(transfer.reader.read(&mut output).is_err());
    assert_eq!(output, [0x5b; 1024]);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "revoke acknowledgement and next scheduled read took {elapsed:?}"
    );
    elapsed
}

fn resident_bytes(system: &mut sysinfo::System) -> u64 {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).unwrap().memory()
}

fn qualify(frames: u32, spacing_ms: u64) {
    let fixture = Evidence::recording(true, frames, spacing_ms);
    let expected = &fixture.grant.intent.binding;
    let mut system = sysinfo::System::new().unwrap();
    let idle = resident_bytes(&mut system);
    let (authenticated, administrator_rss) =
        record_administrator_download(&fixture, expected, &mut system);
    let baseline = resident_bytes(&mut system);
    let mut high = baseline;
    let mut harness = Harness::with_writer_timeout(Duration::from_secs(90));
    let mut readers = Vec::with_capacity(8);
    let mut first_us = Vec::with_capacity(8);
    let started = Instant::now();
    for index in 0..8 {
        let (bound, handle) = if index == 0 {
            (fixture.bound.clone(), fixture.session.clone())
        } else {
            let (_, token) = grant(fixture.state(), expected.clone(), true);
            exchange(fixture.state(), &fixture.context, &token)
        };
        let (reader, first) = open_download(&fixture, &mut harness, &bound, &handle);
        readers.push(reader);
        first_us.push(first.as_micros());
        high = high.max(resident_bytes(&mut system));
    }
    let revoke = revoke_and_observe(&fixture, readers.pop().unwrap());
    let drain = Instant::now();
    let mut buffer = [0u8; 65_536];
    loop {
        let mut progress = false;
        for reader in &mut readers {
            let count = reader.reader.read(&mut buffer).unwrap();
            reader.digest.update(&buffer[..count]);
            reader.count += count;
            progress |= count > 0;
        }
        if !progress {
            break;
        }
    }
    let drain_elapsed = drain.elapsed();
    let elapsed = started.elapsed();
    for reader in readers {
        assert_eq!(reader.count as u64, expected.bytes);
        assert_eq!(<[u8; 32]>::from(reader.digest.finalize()), expected.sha256);
    }
    high = high.max(resident_bytes(&mut system));
    println!(
        "guest_capacity {}",
        serde_json::json!({"bytes_per_file":expected.bytes, "sessions":8, "completed":7,
        "administrator_prepare_us":authenticated.as_micros(),"guest_first_read_us":first_us,
        "revoke_to_next_read_us":revoke.as_micros(), "seven_downloads_drain_us":drain_elapsed.as_micros(),
        "total_us":elapsed.as_micros(),"idle_rss_bytes":idle,"administrator_result_rss_bytes":administrator_rss,"post_administrator_rss_bytes":baseline,
        "sampled_high_rss_bytes":high})
    );
}

#[test]
fn eight_valid_guest_downloads_keep_the_other_seven_readable_when_one_is_revoked() {
    qualify(2, 1000);
}

#[test]
#[ignore = "run in its own process for guest media timing and resident-memory samples"]
fn measure_eight_guest_downloads() {
    qualify(120, 500);
}

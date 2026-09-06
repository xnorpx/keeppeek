use isapi::{Assembler, Decoder, Part};
use std::time::Duration;

fn part(kind: &str, headers: &str, body: &[u8]) -> Part {
    let mut bytes = format!(
        "--images\r\nContent-Type: {kind}\r\n{headers}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(b"\r\n--images--\r\n");
    let mut decoder = Decoder::new("multipart/form-data; boundary=images").unwrap();
    decoder.push(&bytes).unwrap();
    decoder.next_part().unwrap().unwrap()
}

fn metadata(event_id: &str, image_id: &str) -> Part {
    part("application/json", "", format!(r#"{{"uuid":"{event_id}","eventType":"ANPR","eventState":"active","pictureInfoList":[{{"fileName":"{image_id}"}}]}}"#).as_bytes())
}

fn image(image_id: &str, value: u8) -> Part {
    part(
        "image/jpeg",
        &format!("Content-Disposition: form-data; name=\"image\"; filename=\"{image_id}\"\r\n"),
        &[0xff, 0xd8, value, 0xff, 0xd9],
    )
}

#[test]
fn interleaved_images_before_and_after_metadata_use_explicit_identifiers() {
    let mut assembler = Assembler::new();
    let now = Duration::ZERO;
    assert!(
        assembler
            .push(metadata("one", "one.jpg"), now)
            .unwrap()
            .is_empty()
    );
    assert!(assembler.push(image("two.jpg", 2), now).unwrap().is_empty());
    let second = assembler.push(metadata("two", "two.jpg"), now).unwrap();
    assert_eq!(second[0].event().id(), Some("two"));
    assert_eq!(second[0].images()[0].body()[2], 2);
    let first = assembler.push(image("one.jpg", 1), now).unwrap();
    assert_eq!(first[0].event().id(), Some("one"));
    assert_eq!(first[0].images()[0].id(), "one.jpg");
    assert_eq!(first[0].images()[0].body()[2], 1);
    assert!(first[0].complete());
}

#[test]
fn duplicate_conflicting_and_ambiguous_image_ids_are_rejected() {
    let mut assembler = Assembler::new();
    assembler.push(image("one.jpg", 1), Duration::ZERO).unwrap();
    assert!(
        assembler
            .push(image("one.jpg", 1), Duration::ZERO)
            .unwrap()
            .is_empty()
    );
    assert!(assembler.push(image("one.jpg", 2), Duration::ZERO).is_err());
    let mut assembler = Assembler::new();
    assembler
        .push(metadata("one", "same.jpg"), Duration::ZERO)
        .unwrap();
    assert!(
        assembler
            .push(metadata("two", "same.jpg"), Duration::ZERO)
            .is_err()
    );
}

#[test]
fn missing_images_expire_without_stealing_unrelated_parts_and_reset_drops_orphans() {
    let mut assembler = Assembler::new();
    assembler
        .push(metadata("one", "missing.jpg"), Duration::ZERO)
        .unwrap();
    assembler
        .push(image("unrelated.jpg", 3), Duration::ZERO)
        .unwrap();
    let expired = assembler.expire(Duration::from_secs(6)).unwrap();
    assert_eq!(expired.len(), 1);
    assert!(!expired[0].complete());
    assert!(expired[0].images().is_empty());
    assert!(assembler.finish().is_empty());
    assembler
        .push(metadata("new", "unrelated.jpg"), Duration::from_secs(7))
        .unwrap();
    assert!(assembler.finish()[0].images().is_empty());
}

#[test]
fn content_ids_and_disposition_names_are_opaque_bounded_identifiers() {
    let input = part(
        "image/jpeg",
        "Content-ID: <photo-1>\r\nContent-Disposition: form-data; name=\"snapshot\"; filename=\"../camera.jpg\"\r\n",
        &[0xff, 0xd8, 0xff, 0xd9],
    );
    assert_eq!(input.name(), Some("snapshot"));
    assert_eq!(input.filename(), Some("../camera.jpg"));
    let mut assembler = Assembler::new();
    assembler
        .push(metadata("one", "photo-1"), Duration::ZERO)
        .unwrap();
    assert_eq!(
        assembler.push(input, Duration::ZERO).unwrap()[0].images()[0].id(),
        "photo-1"
    );
    for index in 0..16 {
        assembler
            .push(
                metadata(&format!("e-{index}"), &format!("i-{index}")),
                Duration::from_secs(1),
            )
            .unwrap();
    }
    assert!(
        assembler
            .push(metadata("overflow", "overflow"), Duration::from_secs(1))
            .unwrap_err()
            .is_limit()
    );
}

#[test]
fn clear_cannot_pass_an_active_notification_waiting_for_images() {
    let mut assembler = Assembler::new();
    assembler
        .push(metadata("one", "one.jpg"), Duration::ZERO)
        .unwrap();
    let clear = Part::from_body(
        "application/json",
        br#"{"uuid":"new-notification","eventType":"ANPR","eventState":"inactive"}"#.to_vec(),
    )
    .unwrap();
    assert!(
        assembler
            .push(clear, Duration::from_secs(1))
            .unwrap()
            .is_empty()
    );
    assert_eq!(assembler.next_deadline(), Some(Duration::from_secs(5)));
    let output = assembler.expire(Duration::from_secs(5)).unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(output[0].event().active(), Some(true));
    assert_eq!(output[1].event().active(), Some(false));
    assert_eq!(assembler.next_deadline(), None);
}

#[test]
fn expired_orphans_never_match_new_events_and_expiry_survives_capacity_errors() {
    let mut assembler = Assembler::new();
    assembler.push(image("old.jpg", 1), Duration::ZERO).unwrap();
    assert!(
        assembler
            .push(metadata("new", "old.jpg"), Duration::from_secs(5))
            .unwrap()
            .is_empty()
    );
    assert!(!assembler.finish()[0].complete());
    assembler
        .push(metadata("pending", "missing"), Duration::from_secs(5))
        .unwrap();
    for index in 0..600 {
        let now = Duration::from_secs(5);
        let id = format!("image-{index}");
        if assembler
            .push(metadata(&format!("event-{index}"), &id), now)
            .is_err()
        {
            break;
        }
        if assembler.push(image(&id, 1), now).is_err() {
            break;
        }
    }
    assert!(
        !assembler
            .expire(Duration::from_secs(10))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn new_activity_cannot_overtake_a_queued_clear() {
    let mut assembler = Assembler::new();
    assembler
        .push(metadata("first", "photo"), Duration::ZERO)
        .unwrap();
    for (id, state) in [("clear", "inactive"), ("new", "active")] {
        let part = Part::from_body(
            "application/json",
            format!(r#"{{"uuid":"{id}","eventType":"ANPR","eventState":"{state}"}}"#).into_bytes(),
        )
        .unwrap();
        assert!(
            assembler
                .push(part, Duration::from_secs(1))
                .unwrap()
                .is_empty()
        );
    }
    let bundles = assembler
        .push(image("photo", 1), Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        bundles
            .iter()
            .map(|bundle| bundle.event().id().unwrap())
            .collect::<Vec<_>>(),
        ["first", "clear", "new"]
    );
}

#[test]
fn image_aliases_cannot_be_claimed_by_different_events() {
    let mut assembler = Assembler::new();
    assembler
        .push(
            part(
                "image/jpeg",
                "Content-ID: <first>\r\nContent-Disposition: attachment; filename=\"second\"\r\n",
                &[0xff, 0xd8, 0xff, 0xd9],
            ),
            Duration::ZERO,
        )
        .unwrap();
    let event = Part::from_body("application/json", br#"{"uuid":"one","eventType":"ANPR","eventState":"active","pictureInfoList":[{"fileName":"first"},{"fileName":"missing"}]}"#.to_vec()).unwrap();
    assembler.push(event, Duration::ZERO).unwrap();
    assert!(
        assembler
            .push(metadata("two", "second"), Duration::ZERO)
            .is_err()
    );
}

#[test]
fn expiry_schedule_does_not_change_which_old_images_can_be_attached() {
    for intermediate in [false, true] {
        let mut assembler = Assembler::new();
        assembler.push(image("photo", 1), Duration::ZERO).unwrap();
        let event = Part::from_body("application/json", br#"{"uuid":"one","eventType":"ANPR","eventState":"active","pictureInfoList":[{"fileName":"photo"},{"fileName":"missing"}]}"#.to_vec()).unwrap();
        assembler.push(event, Duration::from_secs(1)).unwrap();
        if intermediate {
            assert!(assembler.expire(Duration::from_secs(5)).unwrap().is_empty());
        }
        let bundles = assembler.expire(Duration::from_secs(6)).unwrap();
        assert_eq!(bundles.len(), 1);
        assert!(bundles[0].images().is_empty());
    }
}

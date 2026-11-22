use super::*;

fn fixture() -> Fixture {
    fixture_with_last_audio(1_980)
}

fn fixture_with_last_audio(late_audio_ms: u64) -> Fixture {
    let mut fixture = Fixture::new(Some(48_000), 0, 0);
    let path = fixture.originals[0].0.clone();
    let fragments = write_source(&path, 0, Some(48_000), 0, 0, 64_000, late_audio_ms);
    fixture.fragments[..2].clone_from_slice(&fragments);
    fixture.originals[0].1 = Sha256::digest(std::fs::read(path).unwrap()).into();
    fixture
}

fn export(fixture: &Fixture, sources: ExportSources<'_>, offset: i32, name: &str) -> Output {
    let path = fixture.root.join(name);
    let artifact = export_sources_to_file(sources, &path, offset, || false, |_| {}).unwrap();
    let output = read_output(&path);
    assert_eq!(artifact.bytes, u64::try_from(output.bytes.len()).unwrap());
    fixture.assert_originals();
    output
}

#[test]
fn negative_correction_imports_the_next_recordings_audio_before_the_exclusive_end() {
    let fixture = fixture();
    let sources = ExportSources {
        video_fragments: &fixture.fragments[1..2],
        audio_context: &fixture.fragments[1..3],
        requested_end_ms: 12_000,
    };
    let baseline = export(&fixture, sources, 0, "negative-baseline.mp4");
    let corrected = export(&fixture, sources, -200, "negative-context.mp4");
    assert_eq!(corrected.video, baseline.video);
    assert_eq!(corrected.video.len(), 1);
    assert_eq!(
        audio_timeline(&corrected),
        expected_audio(48_000, &[(780, 20, 3), (800, 200, 4)])
    );
    assert!(corrected.audio.iter().all(|sample| sample.bytes[0] != 5));
}

#[test]
fn positive_correction_imports_previous_recording_audio_without_duplicate_video() {
    let fixture = fixture();
    let mut context = fixture.fragments[1..3].to_vec();
    context.push(fixture.fragments[2].clone());
    let sources = ExportSources {
        video_fragments: &fixture.fragments[2..3],
        audio_context: &context,
        requested_end_ms: 13_000,
    };
    let baseline = export(&fixture, sources, 0, "positive-baseline.mp4");
    let corrected = export(&fixture, sources, 200, "positive-context.mp4");
    assert_eq!(corrected.video, baseline.video);
    assert_eq!(corrected.video.len(), 1);
    assert_eq!(
        audio_timeline(&corrected),
        expected_audio(48_000, &[(180, 20, 3), (200, 200, 4), (400, 20, 5)])
    );
}

#[test]
fn audio_mapped_after_the_output_window_is_rejected_before_publication() {
    let fixture = fixture();
    let destination = fixture.root.join("all-after-end.mp4");
    let result = export_sources_to_file(
        ExportSources {
            video_fragments: &fixture.fragments[..1],
            audio_context: &fixture.fragments[..1],
            requested_end_ms: 10_150,
        },
        &destination,
        200,
        || false,
        |_| {},
    );
    assert!(result.is_err());
    fixture.assert_no_partial(&destination);
}

#[test]
fn zero_correction_ignores_neighbors_and_matches_the_existing_entrypoint_bytes() {
    let fixture = fixture();
    let sources = ExportSources {
        video_fragments: &fixture.fragments[2..3],
        audio_context: &fixture.fragments,
        requested_end_ms: 13_000,
    };
    let context = export(&fixture, sources, 0, "zero-context.mp4");
    let path = fixture.root.join("zero-existing.mp4");
    export_fragment_ranges(sources.video_fragments, 13_000, &path, || false).unwrap();
    let existing = read_output(&path);
    assert_eq!(context.bytes, existing.bytes);
    assert_eq!(context.video, existing.video);
    assert_eq!(context.audio, existing.audio);
    fixture.assert_originals();
}

#[test]
fn contextual_correction_rejects_aac_composition_offsets_without_publication() {
    for composition in [-480, 480] {
        let fixture = Fixture::new(Some(48_000), 50, composition);
        let destination = fixture.root.join("unsupported-composition.mp4");
        let error = export_sources_to_file(
            ExportSources {
                video_fragments: &fixture.fragments,
                audio_context: &fixture.fragments,
                requested_end_ms: 14_000,
            },
            &destination,
            100,
            || false,
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("AAC composition offsets"));
        fixture.assert_no_partial(&destination);
    }
}

#[test]
fn corrected_context_does_not_stretch_audio_packets_across_a_source_gap() {
    let fixture = fixture_with_last_audio(1_850);
    let destination = fixture.root.join("unsupported-gap.mp4");
    let error = export_sources_to_file(
        ExportSources {
            video_fragments: &fixture.fragments[2..3],
            audio_context: &fixture.fragments[1..3],
            requested_end_ms: 13_000,
        },
        &destination,
        200,
        || false,
        |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("gap or overlap"));
    fixture.assert_no_partial(&destination);
}

#[test]
fn contextual_correction_rejects_overlapping_recordings_before_audio_is_discarded() {
    let fixture = fixture();
    let mut fragments = fixture.fragments.clone();
    for fragment in &mut fragments[2..] {
        fragment.recording_started_at_ms -= 1_000;
        fragment.start_ms -= 1_000;
    }
    let sources = ExportSources {
        video_fragments: &fragments,
        audio_context: &fragments,
        requested_end_ms: 13_000,
    };
    let baseline = export(&fixture, sources, 0, "zero-overlap.mp4");
    assert_eq!(baseline.video.len(), 3);
    let destination = fixture.root.join("corrected-overlap.mp4");
    let error = export_sources_to_file(sources, &destination, 100, || false, |_| {}).unwrap_err();
    assert!(error.to_string().contains("overlap"));
    fixture.assert_no_partial(&destination);
}

#[test]
fn contextual_plan_accepts_exact_duplicates_but_rejects_conflicting_period_metadata() {
    let fixture = fixture();
    let video = &fixture.fragments[2..3];
    let mut context = vec![video[0].clone(), video[0].clone()];
    assert!(
        export_recordings(
            ExportSources {
                video_fragments: video,
                audio_context: &context,
                requested_end_ms: 13_000,
            },
            200
        )
        .is_ok()
    );
    context[1].byte_offset += 1;
    assert!(
        export_recordings(
            ExportSources {
                video_fragments: video,
                audio_context: &context,
                requested_end_ms: 13_000,
            },
            200
        )
        .is_err()
    );
    fixture.assert_originals();
}

#[test]
fn contextual_plan_bounds_the_unique_union_instead_of_only_each_input_slice() {
    let fixture = fixture();
    let base = &fixture.fragments[0];
    let periods = (0..513_u64)
        .map(|index| CatalogMediaFragment {
            sequence: index + 1,
            start_ms: base.start_ms + i64::try_from(index).unwrap() * 20,
            duration_ms: 20,
            byte_offset: base.byte_offset + index * base.byte_len,
            ..base.clone()
        })
        .collect::<Vec<_>>();
    let bounded = ExportSources {
        video_fragments: &periods[..256],
        audio_context: &periods[..512],
        requested_end_ms: 30_000,
    };
    assert!(export_recordings(bounded, 200).is_ok());
    let excessive = ExportSources {
        video_fragments: &periods[256..],
        audio_context: &periods[..256],
        requested_end_ms: 30_000,
    };
    assert!(export_recordings(excessive, 200).is_err());
    assert!(export_recordings(excessive, 0).is_ok());
    fixture.assert_originals();
}

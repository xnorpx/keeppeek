//! Seeds a realistic multi-hour recorded database with fake bunny detections for playback benchmarks.

use anyhow::{Context, bail, ensure};
use image::{Rgb, RgbImage, codecs::jpeg::JpegEncoder};
use keeppeek::{
    config::{StorageToml, load_cameras},
    storage::{
        CatalogFragment, CatalogKeyframe, CatalogRecording, EventSearchField, EventSearchTerm,
        EventStore, RecordingCatalog, RecordingCatalogHandle, RecordingStreamIdentity,
        StorageConfig,
        catalog::locations::{Kind, Object},
        metadata::{EventAttachment, EventSource, TimelineEvent},
        volumes::VolumeRole,
    },
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    time::Instant,
};

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
const FIVE_MINUTES_MS: i64 = 300_000;
const HERO_HOUR: u32 = 14;
const BLOCK_STEP_HOURS: u32 = 2;
const STANDARD_GOP_STRETCH: u32 = 5;
const STANDARD_GOPS: u32 = 60;
const HERO_FAST_SECONDS: u32 = 60;
const HERO_TAIL_GOPS: u32 = 48;
const PREVIOUS_DAY_HOURS: &[u32] = &[8, 16];
const MAIN_STREAM_HOURS: &[u32] = &[10, HERO_HOUR];
const THUMBNAIL_WIDTH: u32 = 320;
const THUMBNAIL_HEIGHT: u32 = 180;
const MAX_HOURS: u32 = 24;
const MAX_EVENTS_PER_CAMERA: u32 = 1_000;

/// Options controlling deterministic playback corpus generation.
pub struct PlaybackCorpusOptions {
    pub source: PathBuf,
    pub config_path: PathBuf,
    pub storage: StorageToml,
    pub date: String,
    pub hours: u32,
    pub events_per_camera: u32,
}

struct PreparedFragment {
    info: mp4::Mp4FragmentInfo,
    start_ms: i64,
    end_ms: i64,
}

struct PreparedSegment {
    bytes: Vec<u8>,
    initialization_len: u64,
    duration_ms: i64,
    fragments: Vec<PreparedFragment>,
}

struct SeededCamera {
    source_id: String,
    label: String,
    display_name: String,
    zones: &'static [&'static str],
}

#[derive(Clone, Copy)]
struct ZoneTheme {
    name: &'static str,
    sky: [u8; 3],
    ground: [u8; 3],
    accent: [u8; 3],
    bunny_coat: [u8; 3],
}

const BACKYARD_ZONES: &[&str] = &["garden-bed", "clover-patch", "patio", "carrot-planter"];
const FRONT_ZONES: &[&str] = &["front-lawn", "driveway", "porch", "hedge-row"];

const ZONE_THEMES: &[ZoneTheme] = &[
    ZoneTheme {
        name: "garden-bed",
        sky: [110, 168, 224],
        ground: [64, 118, 56],
        accent: [120, 78, 48],
        bunny_coat: [232, 224, 212],
    },
    ZoneTheme {
        name: "clover-patch",
        sky: [124, 184, 232],
        ground: [52, 134, 62],
        accent: [158, 214, 118],
        bunny_coat: [246, 244, 238],
    },
    ZoneTheme {
        name: "patio",
        sky: [132, 174, 214],
        ground: [156, 126, 102],
        accent: [194, 140, 104],
        bunny_coat: [208, 194, 176],
    },
    ZoneTheme {
        name: "carrot-planter",
        sky: [118, 176, 228],
        ground: [72, 114, 52],
        accent: [224, 122, 42],
        bunny_coat: [236, 232, 224],
    },
    ZoneTheme {
        name: "front-lawn",
        sky: [126, 186, 236],
        ground: [68, 142, 64],
        accent: [98, 166, 86],
        bunny_coat: [176, 144, 114],
    },
    ZoneTheme {
        name: "driveway",
        sky: [136, 172, 208],
        ground: [84, 88, 96],
        accent: [142, 148, 158],
        bunny_coat: [242, 240, 236],
    },
    ZoneTheme {
        name: "porch",
        sky: [118, 162, 206],
        ground: [132, 96, 68],
        accent: [174, 132, 96],
        bunny_coat: [244, 238, 228],
    },
    ZoneTheme {
        name: "hedge-row",
        sky: [108, 160, 212],
        ground: [48, 96, 50],
        accent: [34, 74, 38],
        bunny_coat: [188, 164, 138],
    },
];

const BUNNY_BREEDS: &[&str] = &[
    "cottontail",
    "white",
    "spotted",
    "fluffy",
    "brown",
    "lop-eared",
];

const BUNNY_ACTIONS: &[&str] = &[
    "nibbling clover near",
    "hopping across",
    "resting beside",
    "inspecting",
    "sprinting past",
    "peeking around",
];

const OTHER_EVENTS: &[(&str, &str, &str, &str)] = &[
    ("person", "person", "Person walking past", "person"),
    ("vehicle", "vehicle", "Vehicle moving near", "vehicle"),
    ("motion", "motion", "Motion detected around", "motion"),
];

struct SourceTrack {
    track_config: mp4::TrackConfig,
    timescale: u32,
    samples: Vec<mp4::Mp4Sample>,
}

/// Seeds continuous 5-minute 640x360 H.264 recordings, bunny detections, thumbnails, search terms, and keyframe links.
pub fn seed_playback_corpus(options: &PlaybackCorpusOptions) -> anyhow::Result<()> {
    let started = Instant::now();
    ensure!(
        (1..=MAX_HOURS).contains(&options.hours),
        "hours must be between 1 and {MAX_HOURS}"
    );
    ensure!(
        (1..=MAX_EVENTS_PER_CAMERA).contains(&options.events_per_camera),
        "events-per-camera must be between 1 and {MAX_EVENTS_PER_CAMERA}"
    );
    let day_start_ms = parse_utc_date_start_ms(&options.date)?;
    let day_end_ms = day_start_ms + DAY_MS;

    let camera_groups = load_cameras(&options.config_path).with_context(|| {
        format!(
            "unable to load cameras from {}",
            options.config_path.display()
        )
    })?;
    let cameras = collect_cameras(&camera_groups)?;
    ensure!(
        !cameras.is_empty(),
        "playback corpus requires at least one camera in config.toml"
    );

    let source_track = read_source_track(&options.source)?;
    // Every block spans a full 5-minute 24h-zoom availability bucket with continuous H.264 frames.
    let standard_segment = render_segment(&source_track, &[(STANDARD_GOPS, STANDARD_GOP_STRETCH)])?;
    let hero_segment = render_segment(
        &source_track,
        &[
            (HERO_FAST_SECONDS, 1),
            (HERO_TAIL_GOPS, STANDARD_GOP_STRETCH),
        ],
    )?;
    ensure!(
        standard_segment.duration_ms >= FIVE_MINUTES_MS
            && hero_segment.duration_ms >= FIVE_MINUTES_MS,
        "playback corpus segments must cover a full 5-minute timeline bucket"
    );

    let mut storage = StorageConfig::from_toml(&options.storage);
    let binding = storage
        .metadata
        .as_ref()
        .context("playback corpus metadata owner is missing")?;
    let catalog = RecordingCatalog::open_managed(
        &storage.recording_catalog_path,
        &binding.authority(),
        &binding.root_identity(),
    )?;
    storage.initialize_named_volumes(catalog.handle())?;
    {
        let mut groups = storage
            .volume_groups
            .write()
            .map_err(|_| anyhow::anyhow!("volume group registry unavailable"))?;
        for (group, group_cameras) in &camera_groups {
            for camera in group_cameras {
                groups
                    .entry(camera.ip.to_string())
                    .or_default()
                    .push(group.clone());
            }
        }
    }
    let event_store = EventStore::from_storage(catalog.handle(), &storage)?;
    let handle = catalog.handle();

    let active_hours: Vec<u32> = (0..options.hours)
        .step_by(BLOCK_STEP_HOURS as usize)
        .collect();
    let hero_hour = if active_hours.contains(&HERO_HOUR) {
        HERO_HOUR
    } else {
        *active_hours.last().context("no active recording hours")?
    };

    let mut recording_count = 0_u64;
    let mut fragment_count = 0_u64;

    for camera in &cameras {
        let sub_identity = RecordingStreamIdentity::new(&camera.source_id, "sub", &camera.label);
        for &prev_hour in PREVIOUS_DAY_HOURS {
            let start_ms = day_start_ms - DAY_MS + i64::from(prev_hour) * HOUR_MS;
            publish_segment(
                &storage,
                &handle,
                &standard_segment,
                &sub_identity,
                start_ms,
            )?;
            recording_count += 1;
            fragment_count += u64::try_from(standard_segment.fragments.len())?;
        }
        for &hour in &active_hours {
            let segment = if hour == hero_hour {
                &hero_segment
            } else {
                &standard_segment
            };
            let start_ms = day_start_ms + i64::from(hour) * HOUR_MS;
            publish_segment(&storage, &handle, segment, &sub_identity, start_ms)?;
            recording_count += 1;
            fragment_count += u64::try_from(segment.fragments.len())?;
        }

        let main_identity = RecordingStreamIdentity::new(&camera.source_id, "main", &camera.label);
        for &hour in MAIN_STREAM_HOURS {
            if !active_hours.contains(&hour) {
                continue;
            }
            let segment = if hour == hero_hour {
                &hero_segment
            } else {
                &standard_segment
            };
            let start_ms = day_start_ms + i64::from(hour) * HOUR_MS;
            publish_segment(&storage, &handle, segment, &main_identity, start_ms)?;
            recording_count += 1;
            fragment_count += u64::try_from(segment.fragments.len())?;
        }
    }

    let thumbnails = precompute_thumbnails()?;
    let mut total_events = 0_u64;
    let mut bunny_events = 0_u64;
    let mut thumbnail_count = 0_u64;
    let mut sample_bunny_events = Vec::new();

    for (camera_index, camera) in cameras.iter().enumerate() {
        for event_index in 0..options.events_per_camera {
            let hour = active_hours[event_index as usize % active_hours.len()];
            let slot_in_block = event_index as usize / active_hours.len();
            let offset_ms = 5_000 + ((slot_in_block as i64) * 14_000) % (FIVE_MINUTES_MS - 15_000);
            let start_time_ms = day_start_ms + i64::from(hour) * HOUR_MS + offset_ms;
            let end_time_ms = start_time_ms + 3_000;
            let zone = camera.zones[(event_index as usize + camera_index) % camera.zones.len()];
            let is_bunny = (event_index % 4) != 3;
            let has_thumbnail = (event_index % 10) < 7;
            let event_id = format!(
                "{}-{}-{event_index:04}",
                if is_bunny { "bunny" } else { "event" },
                camera.label
            );

            let (bbox, variant_index) = deterministic_bbox(camera_index, event_index);
            let (kind, icon_key, text, search_terms) = if is_bunny {
                bunny_events += 1;
                let breed =
                    BUNNY_BREEDS[(event_index as usize + camera_index) % BUNNY_BREEDS.len()];
                let action =
                    BUNNY_ACTIONS[(event_index as usize / 2 + camera_index) % BUNNY_ACTIONS.len()];
                let zone_label = zone.replace('-', " ");
                let text = format!("Bunny ({breed}) {action} the {zone_label}");
                let terms = vec![
                    EventSearchTerm {
                        field: EventSearchField::Text,
                        value: text.clone(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::ObjectClass,
                        value: "bunny".to_owned(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::ObjectClass,
                        value: "rabbit".to_owned(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::ObjectClass,
                        value: breed.to_owned(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::Text,
                        value: zone.to_owned(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::Text,
                        value: zone_label,
                    },
                ];
                ("animal", "animal", text, terms)
            } else {
                let (kind, icon_key, prefix, label) =
                    OTHER_EVENTS[(event_index as usize / 4 + camera_index) % OTHER_EVENTS.len()];
                let zone_label = zone.replace('-', " ");
                let text = format!("{prefix} the {zone_label}");
                let terms = vec![
                    EventSearchTerm {
                        field: EventSearchField::Text,
                        value: text.clone(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::ObjectClass,
                        value: label.to_owned(),
                    },
                    EventSearchTerm {
                        field: EventSearchField::Text,
                        value: zone.to_owned(),
                    },
                ];
                (kind, icon_key, text, terms)
            };

            let confidence =
                0.78 + f64::from((event_index * 7 + camera_index as u32 * 5) % 21) * 0.01;
            let jpeg = thumbnails
                .get(&(zone, is_bunny, variant_index))
                .context("precomputed bunny thumbnail is missing")?;
            let mut event = TimelineEvent {
                id: event_id.clone(),
                revision: 1,
                camera_id: camera.source_id.clone(),
                stream: None,
                source: EventSource::KeepPeek,
                kind: kind.to_owned(),
                start_time_ms,
                end_time_ms: Some(end_time_ms),
                confidence: Some((confidence * 100.0).round() / 100.0),
                bbox: Some(bbox),
                bbox_attachment_id: None,
                zone: Some(zone.to_owned()),
                text: Some(text.clone()),
                payload: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: icon_key.to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            };

            if has_thumbnail {
                event.attachments.push(EventAttachment {
                    id: "snapshot".to_owned(),
                    attachment_type: "snapshot".to_owned(),
                    content_type: "image/jpeg".to_owned(),
                    byte_len: Some(u64::try_from(jpeg.len())?),
                    ordinal: 0,
                    timestamp_ms: Some(start_time_ms),
                    text: None,
                });
                event.canonical_attachment_id = Some("snapshot".to_owned());
                event.bbox_attachment_id = Some("snapshot".to_owned());
                event_store.commit_event_image(&format!("seed-{event_id}"), event, jpeg)?;
                thumbnail_count += 1;
            } else {
                event_store.insert(event)?;
            }
            handle.replace_event_search_terms(&event_id, search_terms)?;
            total_events += 1;

            if is_bunny && has_thumbnail && sample_bunny_events.len() < 12 && event_index % 15 == 0
            {
                sample_bunny_events.push(serde_json::json!({
                    "id": event_id,
                    "cameraId": camera.source_id,
                    "cameraLabel": camera.label,
                    "zone": zone,
                    "text": text,
                    "startTimeMs": start_time_ms,
                    "hasThumbnail": has_thumbnail,
                }));
            }
        }
    }

    let scrub_targets = active_hours
        .iter()
        .map(|&hour| day_start_ms + i64::from(hour) * HOUR_MS + FIVE_MINUTES_MS / 2)
        .collect::<Vec<_>>();
    let hero_camera_id = cameras[0].source_id.clone();
    let hero_start_ms = day_start_ms + i64::from(hero_hour) * HOUR_MS;
    let hero_end_ms = hero_start_ms + hero_segment.duration_ms;

    let camera_manifest = cameras
        .iter()
        .map(|camera| {
            serde_json::json!({
                "id": camera.source_id,
                "label": camera.label,
                "displayName": camera.display_name,
                "zones": camera.zones,
            })
        })
        .collect::<Vec<_>>();

    drop(event_store);
    drop(handle);
    drop(storage);
    catalog.shutdown();

    let output = serde_json::json!({
        "date": options.date,
        "dayStartMs": day_start_ms,
        "dayEndMs": day_end_ms,
        "cameras": camera_manifest,
        "recordingCount": recording_count,
        "fragmentCount": fragment_count,
        "eventCount": total_events,
        "bunnyEventCount": bunny_events,
        "thumbnailCount": thumbnail_count,
        "seedDurationMs": started.elapsed().as_millis(),
        "heroSegment": {
            "cameraId": hero_camera_id,
            "startMs": hero_start_ms,
            "endMs": hero_end_ms,
        },
        "scrubTargets": scrub_targets,
        "sampleBunnyEvents": sample_bunny_events,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn collect_cameras(
    groups: &std::collections::HashMap<String, Vec<keeppeek::cameras::CameraConfig>>,
) -> anyhow::Result<Vec<SeededCamera>> {
    let mut seen = BTreeMap::new();
    for cameras in groups.values() {
        for camera in cameras {
            let source_id = camera.ip.to_string();
            let label = camera.name.clone().unwrap_or_else(|| source_id.clone());
            let display_name = camera
                .display_name()
                .map_or_else(|| label.clone(), str::to_owned);
            seen.entry(source_id.clone())
                .or_insert((label, display_name));
        }
    }
    ensure!(
        seen.len() <= 16,
        "playback corpus supports at most 16 cameras"
    );
    Ok(seen
        .into_iter()
        .enumerate()
        .map(|(index, (source_id, (label, display_name)))| SeededCamera {
            source_id,
            label,
            display_name,
            zones: if index % 2 == 0 {
                BACKYARD_ZONES
            } else {
                FRONT_ZONES
            },
        })
        .collect())
}

fn read_source_track(path: &Path) -> anyhow::Result<SourceTrack> {
    let mut reader = mp4::read_mp4(File::open(path)?)?;
    let (track_id, track_config, timescale, sample_count) = {
        let (&track_id, track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.media_type().ok() == Some(mp4::MediaType::H264))
            .context("playback corpus source has no H.264 video track")?;
        let track_config = mp4::TrackConfig {
            track_type: track.track_type()?,
            timescale: track.timescale(),
            language: track.language().to_owned(),
            media_conf: track.media_config_for_description(1)?,
        };
        let sample_count = track.sample_count();
        ensure!(
            track.timescale() > 0 && sample_count > 0,
            "playback corpus source has no timed video samples"
        );
        (track_id, track_config, track.timescale(), sample_count)
    };

    let mut samples = Vec::with_capacity(usize::try_from(sample_count)?);
    for sample_id in 1..=sample_count {
        let Some(sample) = reader.read_sample(track_id, sample_id)? else {
            continue;
        };
        if samples.is_empty() && !sample.is_sync {
            continue;
        }
        samples.push(sample);
    }
    ensure!(
        !samples.is_empty(),
        "playback corpus source has no keyframe-aligned samples"
    );
    Ok(SourceTrack {
        track_config,
        timescale,
        samples,
    })
}

fn render_segment(source: &SourceTrack, runs: &[(u32, u32)]) -> anyhow::Result<PreparedSegment> {
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse()?,
        minor_version: 1,
        compatible_brands: vec!["iso6".parse()?],
        timescale: 1_000,
    };
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        Cursor::new(Vec::new()),
        &config,
        std::slice::from_ref(&source.track_config),
    )?;
    let initialization = writer.initialization();
    let total_gops: usize = runs.iter().map(|(count, _)| *count as usize).sum();
    let mut fragments = Vec::with_capacity(total_gops);
    let mut fragment_start_ticks = 0_u64;
    let mut cursor_ticks = 0_u64;

    for &(gop_count, stretch) in runs {
        for _ in 0..gop_count {
            for base in &source.samples {
                if base.is_sync && writer.has_pending_samples() {
                    let info = writer
                        .flush_fragment_before_sample(1, cursor_ticks)?
                        .context("playback corpus GOP is empty")?;
                    fragments.push(PreparedFragment {
                        info,
                        start_ms: ticks_to_ms(fragment_start_ticks, source.timescale)?,
                        end_ms: ticks_to_ms(cursor_ticks, source.timescale)?,
                    });
                    fragment_start_ticks = cursor_ticks;
                }
                let duration = base
                    .duration
                    .checked_mul(stretch)
                    .context("stretched sample duration overflowed")?;
                let sample = mp4::Mp4Sample {
                    start_time: cursor_ticks,
                    duration,
                    rendering_offset: base.rendering_offset,
                    is_sync: base.is_sync,
                    bytes: base.bytes.clone(),
                };
                writer.write_sample(1, sample)?;
                cursor_ticks = cursor_ticks
                    .checked_add(u64::from(duration))
                    .context("segment decode time overflowed")?;
            }
        }
    }
    let last_info = writer
        .write_end()?
        .context("final playback corpus GOP is empty")?;
    let duration_ms = ticks_to_ms(cursor_ticks, source.timescale)?;
    fragments.push(PreparedFragment {
        info: last_info,
        start_ms: ticks_to_ms(fragment_start_ticks, source.timescale)?,
        end_ms: duration_ms,
    });

    Ok(PreparedSegment {
        bytes: writer.into_writer().into_inner(),
        initialization_len: initialization.size,
        duration_ms,
        fragments,
    })
}

fn publish_segment(
    storage: &StorageConfig,
    catalog: &RecordingCatalogHandle,
    segment: &PreparedSegment,
    identity: &RecordingStreamIdentity,
    origin_ms: i64,
) -> anyhow::Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let reservation = storage
        .volume_runtime
        .as_ref()
        .context("playback corpus volume runtime is missing")?
        .reserve(
            VolumeRole::Active,
            &identity.source_id,
            &[],
            Object {
                kind: Kind::Recording,
                id: id.clone(),
            },
            u64::try_from(segment.bytes.len())?,
        )?
        .context("playback corpus placement is unavailable")?;
    let path = reservation.path().to_path_buf();
    let mut file = reservation.open()?;
    file.write_all(&segment.bytes)?;

    catalog.upsert_recording(CatalogRecording {
        id: id.clone(),
        stream_id: identity.storage_key.clone(),
        source_id: Some(identity.source_id.clone()),
        logical_stream_id: Some(identity.stream_id.clone()),
        started_at_ms: origin_ms,
        ended_at_ms: Some(origin_ms + segment.duration_ms),
        path: path
            .to_str()
            .context("recording path is not valid UTF-8")?
            .to_owned(),
        init_offset: 0,
        init_len: segment.initialization_len,
        finalized: false,
    })?;
    let mut batch = Vec::with_capacity(segment.fragments.len());
    for fragment in &segment.fragments {
        let keyframe = fragment
            .info
            .video_keyframe
            .context("playback corpus fragment has no video keyframe")?;
        batch.push((
            CatalogFragment {
                recording_id: id.clone(),
                sequence: u64::from(fragment.info.sequence_number),
                start_ms: origin_ms + fragment.start_ms,
                duration_ms: u64::try_from(fragment.end_ms - fragment.start_ms)?,
                byte_offset: fragment.info.range.offset,
                byte_len: fragment.info.range.size,
                random_access: true,
            },
            CatalogKeyframe {
                recording_id: id.clone(),
                fragment_sequence: u64::from(fragment.info.sequence_number),
                byte_offset: keyframe.offset,
                byte_len: u64::from(keyframe.size),
            },
        ));
    }
    catalog.insert_fragments_with_keyframes(batch)?;
    let evidence = file.evidence()?;
    file.finalize(evidence)?;
    Ok(())
}

type ThumbnailCache = BTreeMap<(&'static str, bool, usize), Vec<u8>>;

const fn deterministic_bbox(camera_index: usize, event_index: u32) -> ([f32; 4], usize) {
    const POSITIONS: [[f32; 4]; 2] = [[0.18, 0.44, 0.22, 0.30], [0.56, 0.42, 0.22, 0.30]];
    let variant = (event_index as usize + camera_index) % POSITIONS.len();
    (POSITIONS[variant], variant)
}

fn precompute_thumbnails() -> anyhow::Result<ThumbnailCache> {
    let mut map = BTreeMap::new();
    for theme in ZONE_THEMES {
        for is_bunny in [true, false] {
            for variant in 0..2 {
                let (bbox, _) = deterministic_bbox(variant, 0);
                let jpeg = render_scene_jpeg(*theme, bbox, is_bunny)?;
                map.insert((theme.name, is_bunny, variant), jpeg);
            }
        }
    }
    Ok(map)
}

fn render_scene_jpeg(theme: ZoneTheme, bbox: [f32; 4], is_bunny: bool) -> anyhow::Result<Vec<u8>> {
    let mut img = RgbImage::new(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    let horizon = THUMBNAIL_HEIGHT * 2 / 5;
    for y in 0..THUMBNAIL_HEIGHT {
        for x in 0..THUMBNAIL_WIDTH {
            let color = if y < horizon {
                let shade = ((y * 18) / horizon) as u8;
                [
                    theme.sky[0].saturating_add(shade),
                    theme.sky[1].saturating_add(shade),
                    theme.sky[2].saturating_add(shade / 2),
                ]
            } else if y < horizon + 12 {
                theme.accent
            } else {
                let stripe = if ((x / 16) + (y / 16)) % 2 == 0 { 0 } else { 8 };
                [
                    theme.ground[0].saturating_add(stripe),
                    theme.ground[1].saturating_add(stripe),
                    theme.ground[2].saturating_add(stripe),
                ]
            };
            img.put_pixel(x, y, Rgb(color));
        }
    }

    let left = (bbox[0] * THUMBNAIL_WIDTH as f32) as i32;
    let top = (bbox[1] * THUMBNAIL_HEIGHT as f32) as i32;
    let width = (bbox[2] * THUMBNAIL_WIDTH as f32) as i32;
    let height = (bbox[3] * THUMBNAIL_HEIGHT as f32) as i32;
    let cx = left + width / 2;
    let cy = top + height / 2;

    if is_bunny {
        draw_ellipse(
            &mut img,
            cx - width / 10,
            cy + height / 6,
            width / 3,
            height / 4,
            theme.bunny_coat,
        );
        draw_ellipse(
            &mut img,
            cx + width / 5,
            cy - height / 10,
            width / 6,
            height / 6,
            theme.bunny_coat,
        );
        draw_ellipse(
            &mut img,
            cx + width / 8,
            cy - height / 3,
            width / 14,
            height / 5,
            theme.bunny_coat,
        );
        draw_ellipse(
            &mut img,
            cx + width / 4,
            cy - height / 3,
            width / 14,
            height / 5,
            theme.bunny_coat,
        );
        draw_ellipse(
            &mut img,
            cx + width / 4,
            cy - height / 3,
            width / 24,
            height / 8,
            [236, 174, 184],
        );
        draw_ellipse(
            &mut img,
            cx - width * 2 / 5,
            cy + height / 6,
            width / 10,
            height / 10,
            [250, 250, 248],
        );
        draw_ellipse(
            &mut img,
            cx + width / 4,
            cy - height / 8,
            2,
            2,
            [28, 24, 24],
        );
    } else {
        draw_ellipse(&mut img, cx, cy, width / 3, height / 3, [64, 72, 86]);
    }

    let mut encoded = Vec::with_capacity(4_096);
    JpegEncoder::new_with_quality(&mut encoded, 78).encode_image(&img)?;
    Ok(encoded)
}

fn draw_ellipse(img: &mut RgbImage, cx: i32, cy: i32, rx: i32, ry: i32, color: [u8; 3]) {
    let rx = rx.max(1);
    let ry = ry.max(1);
    let min_x = (cx - rx).max(0);
    let max_x = (cx + rx).min(THUMBNAIL_WIDTH as i32 - 1);
    let min_y = (cy - ry).max(0);
    let max_y = (cy + ry).min(THUMBNAIL_HEIGHT as i32 - 1);
    let rx2 = i64::from(rx) * i64::from(rx);
    let ry2 = i64::from(ry) * i64::from(ry);
    for y in min_y..=max_y {
        let dy = i64::from(y - cy);
        for x in min_x..=max_x {
            let dx = i64::from(x - cx);
            if dx * dx * ry2 + dy * dy * rx2 <= rx2 * ry2 {
                img.put_pixel(x as u32, y as u32, Rgb(color));
            }
        }
    }
}

fn ticks_to_ms(ticks: u64, timescale: u32) -> anyhow::Result<i64> {
    let ms = u128::from(ticks)
        .checked_mul(1_000)
        .context("timestamp ticks overflowed")?
        / u128::from(timescale);
    Ok(i64::try_from(ms)?)
}

fn parse_utc_date_start_ms(date: &str) -> anyhow::Result<i64> {
    let mut parts = date.split('-');
    let year: i64 = parts
        .next()
        .context("date must be YYYY-MM-DD")?
        .parse()
        .context("invalid year")?;
    let month: i64 = parts
        .next()
        .context("date must be YYYY-MM-DD")?
        .parse()
        .context("invalid month")?;
    let day: i64 = parts
        .next()
        .context("date must be YYYY-MM-DD")?
        .parse()
        .context("invalid day")?;
    if parts.next().is_some()
        || !(1970..=2100).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        bail!("date '{date}' must be a valid YYYY-MM-DD UTC date");
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let yoe = adjusted_year.rem_euclid(400);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146_097 + doe - 719_468;
    Ok(days_since_epoch * DAY_MS)
}

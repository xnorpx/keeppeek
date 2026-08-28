export interface ProfileSummary {
	name: string;
	stream: 'main' | 'sub';
	encoding: string | null;
	resolution: string | null;
	framerate: number | null;
	bitrate_kbps?: number | null;
	quality_rank?: number | null;
	recorded_content_type?: string | null;
	gop?: number | null;
	h264_profile?: string | null;
	audio?: AudioProfileSummary | null;
}

export interface AudioProfileSummary {
	encoding: string;
	sample_rate: number | null;
	bitrate_kbps: number | null;
}

export interface CameraPorts {
	http: number | null;
	https: number | null;
	rtsp: number | null;
	onvif: number | null;
}

export interface CameraCapabilities {
	ptz: boolean;
	audio: boolean;
	events: boolean;
	recording: boolean;
	analytics: boolean;
	imaging: boolean;
	two_way_audio: boolean;
}

export interface MotionDetection {
	supported: boolean;
	controllable: boolean;
	enabled: boolean | null;
	error: string | null;
}

export interface CameraListItem {
	id: string;
	ip: string;
	name: string | null;
	manufacturer: string | null;
	model: string | null;
	firmware_version: string | null;
	serial_number?: string | null;
	hardware_id?: string | null;
	hostname?: string | null;
	mac_address?: string | null;
	is_reolink: boolean;
	backend?: string;
	transport?: string;
	web_url?: string;
	ports?: CameraPorts;
	capabilities?: CameraCapabilities;
	profiles: ProfileSummary[];
}

export interface CameraDetailsResponse {
	camera: CameraListItem;
	health: CameraHealth | null;
	motion_detection: MotionDetection;
}

export interface RecordingSegment {
	stream: 'main' | 'sub';
	date: string;
	hour: string;
	filename: string;
	url: string;
	start_time_ms: number;
	end_time_ms: number;
	duration_ms: number;
}

export interface RecordingsResponse {
	camera_id: string;
	date: string | null;
	dates: string[];
	segments: RecordingSegment[];
}

export type EventIconKey =
	| 'event'
	| 'person'
	| 'vehicle'
	| 'animal'
	| 'package'
	| 'motion'
	| 'doorbell'
	| 'sound'
	| 'story'
	| 'alert';

export type EventImageAvailability = 'none' | 'available' | 'unavailable';

export interface RecordingEvent {
	id: string;
	source_id?: string;
	revision?: number;
	source: 'camera' | 'keeppeek';
	kind: string;
	start_time_ms: number;
	end_time_ms: number | null;
	confidence: number | null;
	bbox: [number, number, number, number] | null;
	bbox_attachment_id?: string | null;
	zone: string | null;
	text?: string | null;
	thumbnail_url: string | null;
	thumbnail_blob?: Blob;
	attachments?: readonly RecordingEventAttachment[];
	canonical_attachment_id?: string | null;
	icon_key?: EventIconKey;
	rejected_icon_key?: string | null;
	image_availability?: EventImageAvailability;
}

export interface RecordingEventAttachment {
	id: string;
	type: string;
	content_type: string;
	byte_length: number | null;
	ordinal: number;
	timestamp_ms: number | null;
	text?: string | null;
}

export interface RecordingEventsResponse {
	camera_id: string;
	date: string;
	events: RecordingEvent[];
}

export interface SanitizedStorage {
	medium_term_path: string;
	long_term_path: string;
	recording_catalog_path: string;
	event_thumbnail_path: string;
	event_thumbnail_max_mb: number;
	short_term_secs: number;
	medium_term_secs: number;
	flush_interval_secs: number;
	write_buffer_bytes: number;
	long_term_max_gb: number;
	minimum_free_gb?: number;
	maximum_used_percent?: number | null;
	warning_free_gb?: number;
	critical_free_gb?: number;
	cleanup_hysteresis_gb?: number;
}

export interface RecordingCapacityEstimate {
	estimated_bitrate_bps: number;
	bytes_per_day: number;
	known_streams: number;
	unknown_streams: number;
	estimated_retention_days: number | null;
}

export interface SanitizedConfig {
	host: string;
	port: number;
	configuration_revision?: string;
	storage: SanitizedStorage;
	camera_count: number;
	recording_estimate: RecordingCapacityEstimate;
}

export interface SettingsConfigUpdate {
	host: string;
	port: number;
	expected_configuration_revision?: string;
	storage: SanitizedStorage;
	move_existing_recordings: boolean;
}

export interface SettingsConfigUpdateResponse {
	config: SanitizedConfig;
	restart_required: boolean;
}

export type CameraBackend = 'auto' | 'retina' | 'reo-proto';

export type CameraTransport = 'tcp' | 'udp';

export type CameraRecordingMode = 'off' | 'sub' | 'main' | 'both' | 'event-boost';

export interface CameraSettings {
	id: string;
	ip: string;
	display_name: string | null;
	manufacturer_override: string | null;
	username_configured: boolean;
	password_configured: boolean;
	onvif_port: number | null;
	http_port: number | null;
	main_rtsp_url: string | null;
	sub_rtsp_url: string | null;
	uid_configured: boolean;
	backend: CameraBackend;
	transport: CameraTransport;
	record_generic_motion_events: boolean;
	recording_mode: CameraRecordingMode;
	event_recording_duration_secs: number;
	health: CameraHealth['state'] | null;
	model: string | null;
}

export interface DiscoveredCameraSettings {
	ip: string;
	brand: string;
	name: string | null;
	model: string | null;
	onvif_port: number | null;
	sources: string[];
	configured: boolean;
	health: CameraHealth['state'] | null;
	catalog?: CameraCatalogCamera | null;
}

export interface CameraCatalogInfo {
	version: string;
	tag: string;
	generated_at: string;
	camera_count: number;
	website_url: string;
}

export interface CameraCatalogCamera {
	id: string;
	brand: string;
	model: string;
	aliases: string[];
	camera_type: string;
	resolution_label: string | null;
	megapixels: number | null;
	sensor: string | null;
	field_of_view: string | null;
	night_vision: string | null;
	ip_rating: string | null;
	ik_rating: string | null;
	two_way_audio: boolean | null;
	release_year: number | null;
	community_notes_count: number;
	protocols: string[];
	codecs: string[];
	streams: CameraCatalogStream[];
	sources: string[];
	stream_hints: CameraCatalogStreamHints | null;
}

export interface CameraCatalogStream {
	name: string;
	resolution: string | null;
	fps: number | null;
	codec: string | null;
}

export interface CameraCatalogStreamHints {
	main_rtsp_url: string | null;
	sub_rtsp_url: string | null;
}

export interface CameraStreamProbeResult {
	main_rtsp_url: string | null;
	sub_rtsp_url: string | null;
	onvif_port: number | null;
	manufacturer: string | null;
	model: string | null;
	firmware_version: string | null;
	serial_number: string | null;
	hardware_id: string | null;
	profiles: ProfileSummary[];
	streams: CameraStreamVerification[];
	onvif_error: string | null;
}

export interface CameraStreamVerification {
	stream: 'main' | 'sub';
	verified: boolean;
	codec: string | null;
	resolution: string | null;
	declared_fps: number | null;
	frames_received: number;
	keyframe_received: boolean;
	elapsed_ms: number;
	error: string | null;
}

export interface CameraOnboardingDefaults {
	username_configured: boolean;
	password_configured: boolean;
	networks: CameraDiscoveryNetwork[];
}

export interface CameraDiscoveryNetwork {
	cidr: string;
	interface_name: string;
	preferred: boolean;
}

export interface CameraSettingsUpdate {
	display_name?: string | null;
	manufacturer?: string | null;
	username?: string;
	password?: string;
	onvif_port?: number | null;
	http_port?: number | null;
	main_rtsp_url?: string | null;
	sub_rtsp_url?: string | null;
	uid?: string | null;
	backend?: CameraBackend;
	transport?: CameraTransport;
	record_generic_motion_events?: boolean;
	recording_mode?: CameraRecordingMode;
	event_recording_duration_secs?: number;
}

export interface CameraSettingsUpdateResponse {
	camera: CameraSettings;
	restart_required: boolean;
}

export interface Health {
	status: string;
}

export type LiveQuality = 'auto' | 'high' | 'low';

export interface ServerHealthResponse {
	status: 'healthy' | 'degraded';
	health_contract_version: number;
	generated_at_ms: number;
	uptime_seconds: number;
	version: string;
	totals: HealthTotals;
	system: SystemHealth;
	storage: StorageHealth;
	webrtc: WebRtcHealth;
	cameras: CameraHealth[];
	issues: HealthIssue[];
}

export interface HealthTotals {
	configured_cameras: number;
	connected_cameras: number;
	fresh_cameras: number;
	decodable_cameras: number;
	recording_requested_cameras: number;
	recording_cameras: number;
	unknown_cameras: number;
	configured_video_streams: number;
	connected_video_streams: number;
	fresh_video_streams: number;
	decodable_video_streams: number;
	recording_requested_video_streams: number;
	recording_video_streams: number;
	ingress_fps: number;
	ingress_bitrate_bps: number;
	frames: number;
	keyframes: number;
	drops: number;
	errors: number;
	reconnects: number;
}

export type CameraHealthState =
	| 'starting'
	| 'healthy'
	| 'degraded'
	| 'stale'
	| 'reconnecting'
	| 'offline'
	| 'stopped'
	| 'unknown';

export type CameraHealthReason =
	| 'healthy'
	| 'starting'
	| 'not_expected'
	| 'battery_sleeping'
	| 'evidence_unavailable'
	| 'transport_disconnected'
	| 'transport_reconnecting'
	| 'transport_partially_connected'
	| 'no_stream_report'
	| 'stream_report_stale'
	| 'frames_not_arriving'
	| 'frames_below_expected'
	| 'keyframes_missing'
	| 'ingress_reconnects'
	| 'ingress_drops'
	| 'ingress_errors'
	| 'recording_not_progressing'
	| 'unknown';

export interface CameraHealthDimensions {
	configured: boolean;
	expected: boolean;
	configured_video_streams: number;
	connected_video_streams: number | null;
	reporting_video_streams: number;
	fresh_video_streams: number;
	decodable_video_streams: number;
	configured_video_stream_ids: string[];
	connected_video_stream_ids: string[] | null;
	reporting_video_stream_ids: string[];
	fresh_video_stream_ids: string[];
	decodable_video_stream_ids: string[];
	transport_connected: boolean | null;
	latest_report_at_ms: number | null;
	report_age_ms: number | null;
	frames_fresh: boolean | null;
	decodable: boolean | null;
	recent_reconnects: number;
	recent_drops: number;
	recent_errors: number;
	recording_requested: boolean;
	recording_video_streams: number;
	recording_streams_progressing: number;
	recording_video_stream_ids: string[];
	recording_progressing_stream_ids: string[];
	recording_progressing: boolean | null;
	recording_progress_age_ms: number | null;
	session_duration_ms: number | null;
	recorded_main_duration_ms: number;
	recorded_sub_duration_ms: number;
	recorded_total_duration_ms: number;
	battery_configured: boolean;
	battery_registered: boolean | null;
	battery_last_seen_age_ms: number | null;
	battery_wake_pending_age_ms: number | null;
	battery_sleeping: boolean | null;
}

export interface CameraHealth {
	id: string;
	ip: string;
	name: string;
	manufacturer: string | null;
	model: string | null;
	firmware_version: string | null;
	backend?: string;
	transport?: string;
	state: CameraHealthState;
	reason?: CameraHealthReason;
	reason_codes?: CameraHealthReason[];
	detail?: string;
	dimensions?: CameraHealthDimensions | null;
	lifecycle: string | null;
	last_error: string | null;
	configured_profiles: ProfileSummary[];
	streams: StreamHealth[];
}

export interface StreamHealthDimensions {
	expected: boolean;
	transport_connected: boolean | null;
	report_fresh: boolean;
	report_freshness_threshold_ms: number;
	frames_fresh: boolean;
	frame_freshness_threshold_ms: number;
	decodable: boolean;
	keyframe_freshness_threshold_ms: number;
	recent_reconnects: number;
	recent_drops: number;
	recent_errors: number;
	recording_requested: boolean;
	recording_progressing: boolean | null;
	recording_progress_age_ms: number | null;
	session_duration_ms: number;
	recorded_duration_ms: number;
}

export interface StreamHealth {
	type: string;
	codec?: string;
	resolution?: string;
	fps?: number;
	expected_fps?: number;
	kf_fps?: number;
	kbps?: number;
	max_frame_kb?: number;
	gap_min_ms?: number;
	gap_avg_ms?: number;
	gap_max_ms?: number;
	jitter_samples?: number;
	jitter_p50_ms?: number;
	jitter_p99_ms?: number;
	frames?: number;
	bytes?: number;
	keyframes?: number;
	reconnects?: number;
	drops?: number;
	errors?: number;
	updated_at_ms: number;
	report_age_ms: number;
	frame_updated_at_ms?: number | null;
	frame_age_ms?: number | null;
	keyframe_updated_at_ms?: number | null;
	keyframe_age_ms?: number | null;
	recent_reconnects?: number;
	recent_drops?: number;
	recent_errors?: number;
	state?: CameraHealthState;
	reason?: CameraHealthReason;
	reason_codes?: CameraHealthReason[];
	detail?: string;
	dimensions?: StreamHealthDimensions | null;
}

export interface HealthIssue {
	severity: 'critical' | 'warning' | 'info';
	scope: string;
	message: string;
}

export interface SystemHealth {
	host_name: string | null;
	os_name: string | null;
	os_version: string | null;
	kernel_version: string | null;
	architecture: string;
	system_uptime_seconds: number;
	boot_time_seconds: number;
	logical_cores: number;
	physical_cores: number | null;
	cpu_brand: string | null;
	system_cpu_percent: number;
	process: ProcessHealth;
	memory: MemoryHealth;
	load: LoadHealth;
	cpus: CpuHealth[];
	network_egress_bps: number;
	networks: NetworkHealth[];
	disks: DiskHealth[];
	temperatures: TemperatureHealth[];
}

export interface ProcessHealth {
	pid: number;
	name: string | null;
	executable: string | null;
	working_directory: string | null;
	cpu_percent: number | null;
	cpu_capacity_percent: number | null;
	cpu_core_equivalents: number | null;
	resident_memory_bytes: number | null;
	memory_capacity_percent: number | null;
	virtual_memory_bytes: number | null;
	started_at_seconds: number | null;
	uptime_seconds: number | null;
	tasks: number | null;
	read_bytes_per_second: number | null;
	write_bytes_per_second: number | null;
	total_read_bytes: number | null;
	total_written_bytes: number | null;
}

export interface MemoryHealth {
	total_bytes: number;
	used_bytes: number;
	available_bytes: number;
	total_swap_bytes: number;
	used_swap_bytes: number;
}

export interface LoadHealth {
	one_minute: number;
	five_minutes: number;
	fifteen_minutes: number;
}

export interface CpuHealth {
	name: string;
	usage_percent: number;
	frequency_mhz: number;
}

export interface NetworkHealth {
	name: string;
	received_bytes_per_second: number;
	transmitted_bytes_per_second: number;
	received_packets_per_second: number;
	transmitted_packets_per_second: number;
	receive_errors: number;
	transmit_errors: number;
	total_received_bytes: number;
	total_transmitted_bytes: number;
}

export interface DiskHealth {
	name: string;
	kind: string;
	file_system: string;
	mount_point: string;
	total_bytes: number;
	available_bytes: number;
	used_bytes: number;
	removable: boolean;
	stores_recordings: boolean;
}

export interface TemperatureHealth {
	label: string;
	current_celsius: number | null;
	max_celsius: number | null;
	critical_celsius: number | null;
}

export interface StorageHealth {
	medium_term_path: string;
	long_term_path: string;
	paths_are_same: boolean;
	short_term_seconds: number;
	medium_term_seconds: number;
	flush_interval_seconds: number;
	write_buffer_bytes: number;
	long_term_max_bytes: number;
	minimum_free_bytes?: number;
	maximum_used_percent?: number | null;
	warning_free_bytes?: number;
	critical_free_bytes?: number;
	cleanup_hysteresis_bytes?: number;
	catalog_bytes: number | null;
	catalog: CatalogHealth | null;
	safety?: StorageSafetyHealth | null;
	demand: RecordingDemandHealth;
}

export interface StorageSafetyHealth {
	pressure: 'normal' | 'warning' | 'critical';
	recording_state: 'active' | 'degraded' | 'paused';
	total_bytes: number | null;
	available_bytes: number | null;
	keeppeek_bytes: number | null;
	effective_limit_bytes: number | null;
	cleanup_target_bytes: number | null;
	warning_free_bytes: number;
	critical_free_bytes: number;
	recovery_free_bytes: number;
	last_evaluation_at_ms: number | null;
	last_evaluation_trigger: 'startup' | 'segment_finalized' | 'periodic' | null;
	cleanup_running: boolean;
	last_cleanup_started_at_ms: number | null;
	last_cleanup_ended_at_ms: number | null;
	last_cleanup_files_removed: number;
	last_cleanup_bytes_removed: number;
	last_cleanup_reason: 'archive_cap' | 'filesystem_headroom' | 'combined' | 'reconciliation' | null;
	last_failure_at_ms: number | null;
	last_failure: string | null;
	last_recovered_at_ms: number | null;
}

export interface CatalogHealth {
	recording_files: number;
	finalized_files: number;
	active_files: number;
	protected_files?: number;
	recording_bytes?: number;
	fragments: number;
	fragment_bytes: number;
	events: number;
	open_events: number;
	event_thumbnails: number;
	oldest_recording_at_ms?: number | null;
	newest_recording_at_ms?: number | null;
}

export interface RecordingDemandHealth {
	active_streams: number;
	total_viewers: number;
	leased_streams: number;
	streams: RecordingDemandStreamHealth[];
}

export interface RecordingDemandStreamHealth {
	stream_id: string;
	viewers: number;
	lease_remaining_ms: number | null;
}

export interface WebRtcHealth {
	active_sessions: number;
	adaptive_sessions: number;
	browser_sessions: number;
	browser_tracks: number;
	fixed_sessions: number;
	active_main: number;
	active_sub: number;
	requested_auto: number;
	requested_high: number;
	requested_low: number;
	estimated_bitrate_min_bps: number | null;
	estimated_bitrate_avg_bps: number | null;
	estimated_bitrate_max_bps: number | null;
	source_bitrate_bps: number;
	published_frames: number;
	published_bytes: number;
	delivered_frames: number;
	written_frames: number;
	queue_capacity: number;
	queued_frames: number;
	queue_depth_max: number;
	queue_high_water: number;
	queue_drops: number;
	queue_discarded_frames: number;
	queue_recovery_drops: number;
	session_queues: WebRtcSessionQueueHealth[];
	sources: WebRtcSourceHealth[];
}

export interface WebRtcSessionQueueHealth {
	session_id: number;
	track_id: string | null;
	camera_ip: string;
	stream: 'main' | 'sub';
	depth: number;
	high_water: number;
	written_frames: number;
	full_drops: number;
	discarded_frames: number;
	recovery_drops: number;
}

export interface WebRtcSourceHealth {
	camera_ip: string;
	stream: 'main' | 'sub';
	subscribers: number;
	bitrate_bps: number | null;
	has_keyframe: boolean;
	keyframe_age_ms: number | null;
}

export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error';

export interface ServerLogEntry {
	sequence: number;
	timestamp_ms: number;
	level: LogLevel;
	target: string;
	message: string;
	fields: Record<string, unknown>;
	file?: string;
	line?: number;
}

export interface BrowserLogEntry extends ServerLogEntry {
	source: 'console' | 'window-error' | 'unhandled-rejection';
	stack?: string;
}

export interface LogBufferStats {
	entry_count: number;
	byte_count: number;
	evicted_entries: number;
	max_entries: number;
	max_bytes: number;
	active_streams: number;
	max_streams: number;
}

export interface LogSnapshot {
	entries: ServerLogEntry[];
	oldest_sequence: number | null;
	newest_sequence: number | null;
	truncated: boolean;
	stats: LogBufferStats;
}

export interface LoggingSettings {
	active_filter: string;
	default_filter: string;
	filter_error: string | null;
	version: string;
	buffer: LogBufferStats;
}

export interface CreateRequest {
	offer: SdpOffer;
}

export interface CreateResponse {
	session_id: string;
	answer: SdpAnswer;
}

export interface DeleteRequest {
	session_id: string;
}

export interface SdpOffer {
	type: string;
	sdp: string;
}

export interface SdpAnswer {
	type: string;
	sdp: string;
}

export interface Status {
	code: number;
	message: string;
}

export interface CreateRequest {
	offer: SdpOffer;
}
export interface CreateResponse {
	session_id: string;
	answer: SdpAnswer;
}
export interface DeleteRequest {
	session_id: string;
}
export interface SdpOffer {
	type: string;
	sdp: string;
}
export interface SdpAnswer {
	type: string;
	sdp: string;
}
export interface Status {
	code: number;
	message: string;
}

export interface CreateRequest {
	offer: SdpOffer;
}
export interface CreateResponse {
	session_id: string;
	answer: SdpAnswer;
}
export interface DeleteRequest {
	session_id: string;
}
export interface SdpOffer {
	type: string;
	sdp: string;
}
export interface SdpAnswer {
	type: string;
	sdp: string;
}
export interface Status {
	code: number;
	message: string;
}

export interface CreateRequest {
	offer: SdpOffer;
}
export interface CreateResponse {
	session_id: string;
	answer: SdpAnswer;
}
export interface DeleteRequest {
	session_id: string;
}
export interface SdpOffer {
	type: string;
	sdp: string;
}
export interface SdpAnswer {
	type: string;
	sdp: string;
}
export interface Status {
	code: number;
	message: string;
}

export interface CreateRequest {
	offer: SdpOffer;
}
export interface CreateResponse {
	session_id: string;
	answer: SdpAnswer;
}
export interface DeleteRequest {
	session_id: string;
}
export interface SdpOffer {
	type: string;
	sdp: string;
}
export interface SdpAnswer {
	type: string;
	sdp: string;
}
export interface Status {
	code: number;
	message: string;
}

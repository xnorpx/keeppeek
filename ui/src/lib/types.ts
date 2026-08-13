export interface ProfileSummary {
	name: string;
	stream: 'main' | 'sub';
	encoding: string | null;
	resolution: string | null;
	framerate: number | null;
	bitrate_kbps?: number | null;
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

export interface CameraStatsResponse {
	camera_id: string;
	report: Record<string, unknown> | null;
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

export interface RecordingEvent {
	id: string;
	source: 'camera' | 'keeppeek';
	kind: string;
	start_time_ms: number;
	end_time_ms: number | null;
	confidence: number | null;
	bbox: [number, number, number, number] | null;
	zone: string | null;
	thumbnail_url: string | null;
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
	storage: SanitizedStorage;
	camera_count: number;
	recording_estimate: RecordingCapacityEstimate;
}

export interface SettingsConfigUpdate {
	host: string;
	port: number;
	storage: SanitizedStorage;
	move_existing_recordings: boolean;
}

export interface SettingsConfigUpdateResponse {
	config: SanitizedConfig;
	restart_required: boolean;
}

export type CameraBackend = 'auto' | 'retina' | 'reo-proto';

export type CameraTransport = 'tcp' | 'udp';

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
}

export interface CameraSettingsUpdateResponse {
	camera: CameraSettings;
	restart_required: boolean;
}

export interface RestartResponse {
	restarting: boolean;
}

export interface Health {
	status: string;
}

export type LiveQuality = 'auto' | 'high' | 'low';

export interface LiveSessionStatus {
	requested_quality: LiveQuality;
	active_stream: 'main' | 'sub';
	estimated_bitrate_bps: number | null;
}

export interface LiveSessionResponse extends LiveSessionStatus {
	session_id: number;
	answer: RTCSessionDescriptionInit;
}

export interface BrowserLiveTrackOffer {
	track_id: string;
	camera_id: string;
	mid: string;
	quality: LiveQuality;
}

export interface BrowserLiveTrackStatus extends LiveSessionStatus {
	track_id: string;
}

export interface BrowserLiveSessionStatus {
	estimated_bitrate_bps: number | null;
	tracks: BrowserLiveTrackStatus[];
}

export interface BrowserLiveSessionResponse extends BrowserLiveSessionStatus {
	session_id: number;
	answer: RTCSessionDescriptionInit;
}

export interface ServerHealthResponse {
	status: 'healthy' | 'degraded';
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
	reporting_cameras: number;
	configured_video_streams: number;
	reporting_video_streams: number;
	ingress_fps: number;
	ingress_bitrate_bps: number;
	frames: number;
	keyframes: number;
	drops: number;
	errors: number;
	reconnects: number;
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
	state: 'starting' | 'online' | 'degraded' | 'stale' | 'offline';
	lifecycle: string | null;
	last_error: string | null;
	configured_profiles: ProfileSummary[];
	streams: StreamHealth[];
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
	catalog_bytes: number | null;
	catalog: CatalogHealth | null;
	demand: RecordingDemandHealth;
}

export interface CatalogHealth {
	recording_files: number;
	finalized_files: number;
	active_files: number;
	fragments: number;
	fragment_bytes: number;
	events: number;
	open_events: number;
	event_thumbnails: number;
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

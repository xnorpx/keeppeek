use crate::{
    mp4box::{
        avc1::Avc1Box,
        co64::Co64Box,
        ctts::{CttsBox, CttsEntry},
        hev1::Hev1Box,
        mp4a::Mp4aBox,
        smhd::SmhdBox,
        stco::StcoBox,
        stsc::StscEntry,
        stsd::SampleEntry,
        stss::StssBox,
        stts::SttsEntry,
        traf::TrafBox,
        trak::TrakBox,
        tx3g::Tx3gBox,
        vmhd::VmhdBox,
        vp09::Vp09Box,
    },
    *,
};
use bytes::BytesMut;
use std::{
    cmp,
    io::{Read, Seek, SeekFrom, Write},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackConfig {
    pub track_type: TrackType,
    pub timescale: u32,
    pub language: String,
    pub media_conf: MediaConfig,
}

impl From<MediaConfig> for TrackConfig {
    fn from(media_conf: MediaConfig) -> Self {
        match media_conf {
            MediaConfig::AvcConfig(avc_conf) => Self::from(avc_conf),
            MediaConfig::HevcConfig(hevc_conf) => Self::from(hevc_conf),
            MediaConfig::AacConfig(aac_conf) => Self::from(aac_conf),
            MediaConfig::TtxtConfig(ttxt_conf) => Self::from(ttxt_conf),
            MediaConfig::Vp9Config(vp9_config) => Self::from(vp9_config),
        }
    }
}

impl From<AvcConfig> for TrackConfig {
    fn from(avc_conf: AvcConfig) -> Self {
        Self {
            track_type: TrackType::Video,
            timescale: 1000,               // XXX
            language: String::from("und"), // XXX
            media_conf: MediaConfig::AvcConfig(avc_conf),
        }
    }
}

impl From<HevcConfig> for TrackConfig {
    fn from(hevc_conf: HevcConfig) -> Self {
        Self {
            track_type: TrackType::Video,
            timescale: 1000,               // XXX
            language: String::from("und"), // XXX
            media_conf: MediaConfig::HevcConfig(hevc_conf),
        }
    }
}

impl From<AacConfig> for TrackConfig {
    fn from(aac_conf: AacConfig) -> Self {
        Self {
            track_type: TrackType::Audio,
            timescale: 1000,               // XXX
            language: String::from("und"), // XXX
            media_conf: MediaConfig::AacConfig(aac_conf),
        }
    }
}

impl From<TtxtConfig> for TrackConfig {
    fn from(txtt_conf: TtxtConfig) -> Self {
        Self {
            track_type: TrackType::Subtitle,
            timescale: 1000,               // XXX
            language: String::from("und"), // XXX
            media_conf: MediaConfig::TtxtConfig(txtt_conf),
        }
    }
}

impl From<Vp9Config> for TrackConfig {
    fn from(vp9_conf: Vp9Config) -> Self {
        Self {
            track_type: TrackType::Video,
            timescale: 1000,               // XXX
            language: String::from("und"), // XXX
            media_conf: MediaConfig::Vp9Config(vp9_conf),
        }
    }
}

#[derive(Debug)]
pub struct Mp4Track {
    pub trak: TrakBox,
    pub trafs: Vec<TrafBox>,

    // Fragmented Tracks Defaults.
    pub default_sample_duration: u32,
    pub default_sample_size: u32,
    pub default_sample_flags: u32,
    pub default_sample_description_index: u32,
    pub(crate) traf_sample_counts: Vec<u32>,
    pub(crate) traf_start_times: Vec<u64>,
    pub(crate) traf_base_offsets: Vec<u64>,
    pub(crate) fragmented_duration: u64,
    pub(crate) fragmented_total_size: u64,
}

impl Mp4Track {
    pub(crate) fn from(trak: &TrakBox) -> Self {
        let trak = trak.clone();
        Self {
            trak,
            trafs: Vec::new(),
            default_sample_duration: 0,
            default_sample_size: 0,
            default_sample_flags: 0,
            default_sample_description_index: 1,
            traf_sample_counts: Vec::new(),
            traf_start_times: Vec::new(),
            traf_base_offsets: Vec::new(),
            fragmented_duration: 0,
            fragmented_total_size: 0,
        }
    }

    pub(crate) fn precompute_fragmented_caches(&mut self) {
        let mut running_time = 0u64;
        let mut total_size = 0u64;

        for traf in &self.trafs {
            let start_time = traf
                .tfdt
                .as_ref()
                .map_or(running_time, |tfdt| tfdt.base_media_decode_time);
            self.traf_start_times.push(start_time);

            let default_duration = traf
                .tfhd
                .default_sample_duration
                .unwrap_or(self.default_sample_duration);
            let default_size = traf
                .tfhd
                .default_sample_size
                .unwrap_or(self.default_sample_size);

            let mut traf_duration = 0u64;
            if let Some(trun) = &traf.trun {
                for i in 0..trun.sample_count as usize {
                    traf_duration += trun
                        .sample_durations
                        .get(i)
                        .copied()
                        .unwrap_or(default_duration) as u64;
                    total_size += trun.sample_sizes.get(i).copied().unwrap_or(default_size) as u64;
                }
            }

            running_time = start_time + traf_duration;
        }

        self.fragmented_duration = running_time;
        self.fragmented_total_size = total_size;
    }

    pub const fn track_id(&self) -> u32 {
        self.trak.tkhd.track_id
    }

    pub fn track_type(&self) -> Result<TrackType> {
        TrackType::try_from(&self.trak.mdia.hdlr.handler_type)
    }

    pub fn media_type(&self) -> Result<MediaType> {
        self.media_type_for_description(1)
    }

    pub fn media_type_for_description(&self, sample_description_index: u32) -> Result<MediaType> {
        match self
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .entry(sample_description_index)
        {
            Some(SampleEntry::Avc1(_)) => Ok(MediaType::H264),
            Some(SampleEntry::Hev1(_) | SampleEntry::Hvc1(_)) => Ok(MediaType::H265),
            Some(SampleEntry::Vp09(_)) => Ok(MediaType::VP9),
            Some(SampleEntry::Mp4a(_)) => Ok(MediaType::AAC),
            Some(SampleEntry::Tx3g(_)) => Ok(MediaType::TTXT),
            Some(SampleEntry::Unknown { .. }) => Err(Error::InvalidData("unsupported media type")),
            None => Err(Error::InvalidData(
                "sample description index is out of range",
            )),
        }
    }

    pub fn box_type(&self) -> Result<FourCC> {
        match self.trak.mdia.minf.stbl.stsd.entry(1) {
            Some(SampleEntry::Avc1(_)) => Ok(FourCC::from(BoxType::Avc1Box)),
            Some(SampleEntry::Hev1(_)) => Ok(FourCC::from(BoxType::Hev1Box)),
            Some(SampleEntry::Hvc1(_)) => Ok(FourCC::from(BoxType::Hvc1Box)),
            Some(SampleEntry::Vp09(_)) => Ok(FourCC::from(BoxType::Vp09Box)),
            Some(SampleEntry::Mp4a(_)) => Ok(FourCC::from(BoxType::Mp4aBox)),
            Some(SampleEntry::Tx3g(_)) => Ok(FourCC::from(BoxType::Tx3gBox)),
            Some(SampleEntry::Unknown { .. }) => {
                Err(Error::InvalidData("unsupported sample entry box"))
            }
            None => Err(Error::InvalidData(
                "sample description index is out of range",
            )),
        }
    }

    /// Returns decoder-ready configuration for an H.264 or H.265 video track.
    pub fn video_decoder_config(&self) -> Result<Option<Mp4VideoDecoderConfig>> {
        self.video_decoder_config_for_description(1)
    }

    pub const fn sample_description_count(&self) -> usize {
        self.trak.mdia.minf.stbl.stsd.entries.len()
    }

    pub fn video_decoder_config_for_description(
        &self,
        sample_description_index: u32,
    ) -> Result<Option<Mp4VideoDecoderConfig>> {
        let entry = self
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .entry(sample_description_index)
            .ok_or(Error::InvalidData(
                "sample description index is out of range",
            ))?;
        if let SampleEntry::Avc1(avc1) = entry {
            if avc1.avcc.sequence_parameter_sets.is_empty()
                || avc1.avcc.picture_parameter_sets.is_empty()
            {
                return Err(Error::InvalidData(
                    "AVC decoder configuration has incomplete parameter sets",
                ));
            }
            let mut boxed = Vec::new();
            avc1.avcc.write_box(&mut boxed)?;
            let description = boxed
                .get(HEADER_SIZE as usize..)
                .ok_or(Error::InvalidData("invalid avcC box size"))?
                .to_vec();
            return Ok(Some(Mp4VideoDecoderConfig {
                codec: format!(
                    "avc1.{:02X}{:02X}{:02X}",
                    avc1.avcc.avc_profile_indication,
                    avc1.avcc.profile_compatibility,
                    avc1.avcc.avc_level_indication,
                ),
                width: avc1.width,
                height: avc1.height,
                description,
                nal_length_size: (avc1.avcc.length_size_minus_one & 0x03) + 1,
            }));
        }
        let (sample_entry_name, sample_entry) = match entry {
            SampleEntry::Hvc1(entry) => ("hvc1", entry),
            SampleEntry::Hev1(entry) => ("hev1", entry),
            _ => return Ok(None),
        };
        let configuration = sample_entry.hvcc.configuration()?;
        if configuration.vps.is_empty()
            || configuration.sps.is_empty()
            || configuration.pps.is_empty()
        {
            return Err(Error::InvalidData(
                "HEVC decoder configuration has incomplete parameter sets",
            ));
        }
        Ok(Some(Mp4VideoDecoderConfig {
            codec: hevc_codec_string(sample_entry_name, &sample_entry.hvcc.record_data)?,
            width: sample_entry.width,
            height: sample_entry.height,
            description: sample_entry.hvcc.record_data.clone(),
            nal_length_size: configuration.nal_length_size,
        }))
    }

    pub fn width(&self) -> u16 {
        self.dimensions_for_description(1)
            .map_or_else(|_| self.trak.tkhd.width.value(), |dimensions| dimensions.0)
    }

    pub fn height(&self) -> u16 {
        self.dimensions_for_description(1)
            .map_or_else(|_| self.trak.tkhd.height.value(), |dimensions| dimensions.1)
    }

    pub fn dimensions_for_description(&self, sample_description_index: u32) -> Result<(u16, u16)> {
        match self
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .entry(sample_description_index)
        {
            Some(SampleEntry::Avc1(entry)) => Ok((entry.width, entry.height)),
            Some(SampleEntry::Hev1(entry) | SampleEntry::Hvc1(entry)) => {
                Ok((entry.width, entry.height))
            }
            Some(SampleEntry::Vp09(entry)) => Ok((entry.width, entry.height)),
            Some(_) => Err(Error::InvalidData("sample description is not video")),
            None => Err(Error::InvalidData(
                "sample description index is out of range",
            )),
        }
    }

    pub fn media_config_for_description(
        &self,
        sample_description_index: u32,
    ) -> Result<MediaConfig> {
        match self
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .entry(sample_description_index)
        {
            Some(SampleEntry::Avc1(entry)) => Ok(MediaConfig::AvcConfig(AvcConfig {
                width: entry.width,
                height: entry.height,
                seq_param_set: entry
                    .avcc
                    .sequence_parameter_sets
                    .first()
                    .ok_or(Error::InvalidData("AVC sample description has no SPS"))?
                    .bytes
                    .to_vec(),
                pic_param_set: entry
                    .avcc
                    .picture_parameter_sets
                    .first()
                    .ok_or(Error::InvalidData("AVC sample description has no PPS"))?
                    .bytes
                    .to_vec(),
            })),
            Some(SampleEntry::Hev1(entry) | SampleEntry::Hvc1(entry)) => {
                let configuration = entry.hvcc.configuration()?;
                Ok(MediaConfig::HevcConfig(HevcConfig {
                    width: entry.width,
                    height: entry.height,
                    vps: configuration.vps.first().cloned().unwrap_or_default(),
                    sps: configuration.sps.first().cloned().unwrap_or_default(),
                    pps: configuration.pps.first().cloned().unwrap_or_default(),
                    decoder_config: entry.hvcc.record_data.clone(),
                }))
            }
            Some(SampleEntry::Vp09(entry)) => Ok(MediaConfig::Vp9Config(Vp9Config {
                width: entry.width,
                height: entry.height,
            })),
            Some(SampleEntry::Mp4a(entry)) => {
                let decoder = &entry
                    .esds
                    .as_ref()
                    .ok_or(Error::InvalidData("AAC sample description has no esds"))?
                    .es_desc
                    .dec_config;
                Ok(MediaConfig::AacConfig(AacConfig {
                    bitrate: decoder.avg_bitrate,
                    profile: AudioObjectType::try_from(decoder.dec_specific.profile)?,
                    freq_index: SampleFreqIndex::try_from(decoder.dec_specific.freq_index)?,
                    chan_conf: ChannelConfig::try_from(decoder.dec_specific.chan_conf)?,
                }))
            }
            Some(SampleEntry::Tx3g(_)) => Ok(MediaConfig::TtxtConfig(TtxtConfig::default())),
            Some(SampleEntry::Unknown { .. }) => {
                Err(Error::InvalidData("unsupported sample description"))
            }
            None => Err(Error::InvalidData(
                "sample description index is out of range",
            )),
        }
    }

    pub fn frame_rate(&self) -> f64 {
        let dur_msec = self.duration().as_millis() as u64;
        (self.sample_count() as u64 * 1000)
            .checked_div(dur_msec)
            .unwrap_or(0) as f64
    }

    pub fn sample_freq_index(&self) -> Result<SampleFreqIndex> {
        self.trak.mdia.minf.stbl.stsd.mp4a().map_or_else(
            || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::Mp4aBox)),
            |mp4a| {
                mp4a.esds.as_ref().map_or_else(
                    || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::EsdsBox)),
                    |esds| {
                        SampleFreqIndex::try_from(esds.es_desc.dec_config.dec_specific.freq_index)
                    },
                )
            },
        )
    }

    pub fn channel_config(&self) -> Result<ChannelConfig> {
        self.trak.mdia.minf.stbl.stsd.mp4a().map_or_else(
            || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::Mp4aBox)),
            |mp4a| {
                mp4a.esds.as_ref().map_or_else(
                    || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::EsdsBox)),
                    |esds| ChannelConfig::try_from(esds.es_desc.dec_config.dec_specific.chan_conf),
                )
            },
        )
    }

    pub fn language(&self) -> &str {
        &self.trak.mdia.mdhd.language
    }

    pub const fn timescale(&self) -> u32 {
        self.trak.mdia.mdhd.timescale
    }

    pub const fn duration(&self) -> Duration {
        let duration = if self.trak.mdia.mdhd.duration > 0 {
            self.trak.mdia.mdhd.duration
        } else {
            self.fragmented_duration
        };
        let timescale = self.trak.mdia.mdhd.timescale as u64;
        if timescale == 0 {
            return Duration::ZERO;
        }
        Duration::from_micros(duration * 1_000_000 / timescale)
    }

    pub fn bitrate(&self) -> u32 {
        self.trak.mdia.minf.stbl.stsd.mp4a().map_or_else(
            || {
                let dur_sec = self.duration().as_secs();
                (self.total_sample_size() * 8)
                    .checked_div(dur_sec)
                    .unwrap_or(0) as u32
            },
            |mp4a| {
                mp4a.esds
                    .as_ref()
                    .map_or(0, |esds| esds.es_desc.dec_config.avg_bitrate)
            },
        )
    }

    pub fn sample_count(&self) -> u32 {
        if !self.trafs.is_empty() {
            self.traf_sample_counts.last().copied().unwrap_or(0)
        } else {
            self.trak.mdia.minf.stbl.stsz.sample_count
        }
    }

    pub fn video_profile(&self) -> Result<AvcProfile> {
        self.trak.mdia.minf.stbl.stsd.avc1().map_or_else(
            || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::Avc1Box)),
            |avc1| {
                AvcProfile::try_from((
                    avc1.avcc.avc_profile_indication,
                    avc1.avcc.profile_compatibility,
                ))
            },
        )
    }

    pub fn sequence_parameter_set(&self) -> Result<&[u8]> {
        self.trak.mdia.minf.stbl.stsd.avc1().map_or_else(
            || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::Avc1Box)),
            |avc1| {
                avc1.avcc.sequence_parameter_sets.first().map_or_else(
                    || {
                        Err(Error::EntryInStblNotFound(
                            self.track_id(),
                            BoxType::AvcCBox,
                            0,
                        ))
                    },
                    |nal| Ok(nal.bytes.as_ref()),
                )
            },
        )
    }

    pub fn picture_parameter_set(&self) -> Result<&[u8]> {
        self.trak.mdia.minf.stbl.stsd.avc1().map_or_else(
            || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::Avc1Box)),
            |avc1| {
                avc1.avcc.picture_parameter_sets.first().map_or_else(
                    || {
                        Err(Error::EntryInStblNotFound(
                            self.track_id(),
                            BoxType::AvcCBox,
                            0,
                        ))
                    },
                    |nal| Ok(nal.bytes.as_ref()),
                )
            },
        )
    }

    pub fn audio_profile(&self) -> Result<AudioObjectType> {
        self.trak.mdia.minf.stbl.stsd.mp4a().map_or_else(
            || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::Mp4aBox)),
            |mp4a| {
                mp4a.esds.as_ref().map_or_else(
                    || Err(Error::BoxInStblNotFound(self.track_id(), BoxType::EsdsBox)),
                    |esds| AudioObjectType::try_from(esds.es_desc.dec_config.dec_specific.profile),
                )
            },
        )
    }

    fn stsc_index(&self, sample_id: u32) -> Result<usize> {
        let entries = &self.trak.mdia.minf.stbl.stsc.entries;
        if entries.is_empty() {
            return Err(Error::InvalidData("no stsc entries"));
        }
        match entries.binary_search_by_key(&sample_id, |entry| entry.first_sample) {
            Ok(idx) => Ok(idx),
            Err(idx) => {
                if idx == 0 {
                    Err(Error::InvalidData("sample not found"))
                } else {
                    Ok(idx - 1)
                }
            }
        }
    }

    pub fn sample_description_index(&self, sample_id: u32) -> Result<u32> {
        if sample_id == 0 || sample_id > self.sample_count() {
            return Err(Error::EntryInStblNotFound(
                self.track_id(),
                BoxType::StsdBox,
                sample_id,
            ));
        }
        let index = if !self.trafs.is_empty() {
            let (traf_index, _) = self
                .find_traf_idx_and_sample_idx(sample_id)
                .ok_or_else(|| Error::BoxInTrafNotFound(self.track_id(), BoxType::TrafBox))?;
            self.trafs[traf_index]
                .tfhd
                .sample_description_index
                .unwrap_or(self.default_sample_description_index)
        } else {
            let stsc_index = self.stsc_index(sample_id)?;
            self.trak.mdia.minf.stbl.stsc.entries[stsc_index].sample_description_index
        };
        if index == 0 || index as usize > self.sample_description_count() {
            return Err(Error::InvalidData(
                "sample description index is out of range",
            ));
        }
        Ok(index)
    }

    fn chunk_offset(&self, chunk_id: u32) -> Result<u64> {
        if self.trak.mdia.minf.stbl.stco.is_none() && self.trak.mdia.minf.stbl.co64.is_none() {
            return Err(Error::InvalidData("must have either stco or co64 boxes"));
        }
        if let Some(ref stco) = self.trak.mdia.minf.stbl.stco {
            if let Some(offset) = stco.entries.get(chunk_id as usize - 1) {
                return Ok(*offset as u64);
            } else {
                return Err(Error::EntryInStblNotFound(
                    self.track_id(),
                    BoxType::StcoBox,
                    chunk_id,
                ));
            }
        } else if let Some(ref co64) = self.trak.mdia.minf.stbl.co64 {
            if let Some(offset) = co64.entries.get(chunk_id as usize - 1) {
                return Ok(*offset);
            } else {
                return Err(Error::EntryInStblNotFound(
                    self.track_id(),
                    BoxType::Co64Box,
                    chunk_id,
                ));
            }
        }
        Err(Error::Box2NotFound(BoxType::StcoBox, BoxType::Co64Box))
    }

    fn ctts_index(&self, sample_id: u32) -> Result<(usize, u32)> {
        let ctts = self.trak.mdia.minf.stbl.ctts.as_ref().unwrap();
        let mut sample_count: u32 = 1;
        for (i, entry) in ctts.entries.iter().enumerate() {
            let next_sample_count =
                sample_count
                    .checked_add(entry.sample_count)
                    .ok_or(Error::InvalidData(
                        "attempt to sum ctts entries sample_count with overflow",
                    ))?;
            if sample_id < next_sample_count {
                return Ok((i, sample_count));
            }
            sample_count = next_sample_count;
        }

        Err(Error::EntryInStblNotFound(
            self.track_id(),
            BoxType::CttsBox,
            sample_id,
        ))
    }

    /// return `(traf_idx, sample_idx_in_trun)`
    fn find_traf_idx_and_sample_idx(&self, sample_id: u32) -> Option<(usize, usize)> {
        if self.traf_sample_counts.is_empty() || sample_id == 0 {
            return None;
        }
        let idx = self
            .traf_sample_counts
            .partition_point(|&count| count < sample_id);
        if idx >= self.traf_sample_counts.len() {
            return None;
        }
        let offset = if idx == 0 {
            0
        } else {
            self.traf_sample_counts[idx - 1]
        };
        Some((idx, (sample_id - offset - 1) as usize))
    }

    fn sample_size(&self, sample_id: u32) -> Result<u32> {
        if !self.trafs.is_empty() {
            if let Some((traf_idx, sample_idx)) = self.find_traf_idx_and_sample_idx(sample_id) {
                let traf = &self.trafs[traf_idx];
                if let Some(trun) = &traf.trun
                    && let Some(size) = trun.sample_sizes.get(sample_idx)
                {
                    return Ok(*size);
                }
                if let Some(size) = traf.tfhd.default_sample_size {
                    return Ok(size);
                }
                Ok(self.default_sample_size)
            } else {
                Err(Error::BoxInTrafNotFound(self.track_id(), BoxType::TrafBox))
            }
        } else {
            let stsz = &self.trak.mdia.minf.stbl.stsz;
            if stsz.sample_size > 0 {
                return Ok(stsz.sample_size);
            }
            stsz.sample_sizes.get(sample_id as usize - 1).map_or_else(
                || {
                    Err(Error::EntryInStblNotFound(
                        self.track_id(),
                        BoxType::StszBox,
                        sample_id,
                    ))
                },
                |size| Ok(*size),
            )
        }
    }

    fn total_sample_size(&self) -> u64 {
        if !self.trafs.is_empty() {
            return self.fragmented_total_size;
        }

        let stsz = &self.trak.mdia.minf.stbl.stsz;
        if stsz.sample_size > 0 {
            stsz.sample_size as u64 * self.sample_count() as u64
        } else {
            let mut total_size = 0;
            for size in stsz.sample_sizes.iter() {
                total_size += *size as u64;
            }
            total_size
        }
    }

    fn sample_offset(&self, sample_id: u32) -> Result<u64> {
        if !self.trafs.is_empty() {
            if let Some((traf_idx, sample_idx)) = self.find_traf_idx_and_sample_idx(sample_id) {
                let traf = &self.trafs[traf_idx];
                let mut offset = self
                    .traf_base_offsets
                    .get(traf_idx)
                    .copied()
                    .unwrap_or_else(|| traf.tfhd.base_data_offset.unwrap_or(0));
                if let Some(trun) = &traf.trun {
                    offset = (offset as i64 + trun.data_offset.unwrap_or(0) as i64) as u64;
                    let default_size = traf
                        .tfhd
                        .default_sample_size
                        .unwrap_or(self.default_sample_size);
                    for i in 0..sample_idx {
                        offset += trun.sample_sizes.get(i).copied().unwrap_or(default_size) as u64;
                    }
                }
                Ok(offset)
            } else {
                Err(Error::BoxInTrafNotFound(self.track_id(), BoxType::TrafBox))
            }
        } else {
            let stsc_index = self.stsc_index(sample_id)?;

            let stsc = &self.trak.mdia.minf.stbl.stsc;
            let stsc_entry = stsc.entries.get(stsc_index).unwrap();

            let first_chunk = stsc_entry.first_chunk;
            let first_sample = stsc_entry.first_sample;
            let samples_per_chunk = stsc_entry.samples_per_chunk;

            let chunk_id = sample_id
                .checked_sub(first_sample)
                .map(|n| n / samples_per_chunk)
                .and_then(|n| n.checked_add(first_chunk))
                .ok_or(Error::InvalidData(
                    "attempt to calculate stsc chunk_id with overflow",
                ))?;

            let chunk_offset = self.chunk_offset(chunk_id)?;

            let first_sample_in_chunk = sample_id - (sample_id - first_sample) % samples_per_chunk;

            let stsz = &self.trak.mdia.minf.stbl.stsz;
            let sample_offset = if stsz.sample_size > 0 {
                (sample_id - first_sample_in_chunk) * stsz.sample_size
            } else {
                let start = (first_sample_in_chunk - 1) as usize;
                let end = (sample_id - 1) as usize;
                if end <= stsz.sample_sizes.len() {
                    stsz.sample_sizes[start..end].iter().sum()
                } else {
                    return Err(Error::EntryInStblNotFound(
                        self.track_id(),
                        BoxType::StszBox,
                        sample_id,
                    ));
                }
            };

            Ok(chunk_offset + sample_offset as u64)
        }
    }

    fn sample_time(&self, sample_id: u32) -> Result<(u64, u32)> {
        let stts = &self.trak.mdia.minf.stbl.stts;

        let mut sample_count: u32 = 1;
        let mut elapsed = 0;

        if !self.trafs.is_empty() {
            if let Some((traf_idx, sample_idx)) = self.find_traf_idx_and_sample_idx(sample_id) {
                let traf = &self.trafs[traf_idx];
                let mut start_time = self.traf_start_times[traf_idx];

                let default_duration = traf
                    .tfhd
                    .default_sample_duration
                    .unwrap_or(self.default_sample_duration);
                let mut duration = default_duration;
                if let Some(trun) = &traf.trun {
                    for j in 0..sample_idx {
                        start_time +=
                            trun.sample_durations
                                .get(j)
                                .copied()
                                .unwrap_or(default_duration) as u64;
                    }
                    duration = trun
                        .sample_durations
                        .get(sample_idx)
                        .copied()
                        .unwrap_or(default_duration);
                }
                Ok((start_time, duration))
            } else {
                Err(Error::BoxInTrafNotFound(self.track_id(), BoxType::TrafBox))
            }
        } else {
            for entry in stts.entries.iter() {
                let new_sample_count =
                    sample_count
                        .checked_add(entry.sample_count)
                        .ok_or(Error::InvalidData(
                            "attempt to sum stts entries sample_count with overflow",
                        ))?;
                if sample_id < new_sample_count {
                    let start_time =
                        (sample_id - sample_count) as u64 * entry.sample_delta as u64 + elapsed;
                    return Ok((start_time, entry.sample_delta));
                }

                sample_count = new_sample_count;
                elapsed += entry.sample_count as u64 * entry.sample_delta as u64;
            }

            Err(Error::EntryInStblNotFound(
                self.track_id(),
                BoxType::SttsBox,
                sample_id,
            ))
        }
    }

    fn sample_rendering_offset(&self, sample_id: u32) -> i32 {
        if !self.trafs.is_empty() {
            if let Some((traf_idx, sample_idx)) = self.find_traf_idx_and_sample_idx(sample_id)
                && let Some(trun) = &self.trafs[traf_idx].trun
                && let Some(cts) = trun.sample_cts.get(sample_idx)
            {
                return *cts as i32;
            }
            return 0;
        }

        if let Some(ref ctts) = self.trak.mdia.minf.stbl.ctts
            && let Ok((ctts_index, _)) = self.ctts_index(sample_id)
        {
            let ctts_entry = ctts.entries.get(ctts_index).unwrap();
            return ctts_entry.sample_offset;
        }
        0
    }

    pub(crate) fn is_sync_sample(&self, sample_id: u32) -> bool {
        if !self.trafs.is_empty() {
            if let Some((traf_idx, sample_idx)) = self.find_traf_idx_and_sample_idx(sample_id) {
                let traf = &self.trafs[traf_idx];
                let mut flags = traf
                    .tfhd
                    .default_sample_flags
                    .unwrap_or(self.default_sample_flags);
                if let Some(trun) = &traf.trun {
                    if let Some(sample_flags) = trun.sample_flags.get(sample_idx) {
                        flags = *sample_flags;
                    } else if sample_idx == 0
                        && let Some(first_sample_flags) = trun.first_sample_flags
                    {
                        flags = first_sample_flags;
                    }
                }
                // sample_is_non_sync_sample is bit 16 (0x00010000)
                // If it's 0, it IS a sync sample.
                return (flags & 0x00010000) == 0;
            }
            return false;
        }

        self.trak
            .mdia
            .minf
            .stbl
            .stss
            .as_ref()
            .is_none_or(|stss| stss.entries.binary_search(&sample_id).is_ok())
    }

    pub(crate) fn sample_location(&self, sample_id: u32) -> Result<Option<Mp4SampleLocation>> {
        let offset = match self.sample_offset(sample_id) {
            Ok(offset) => offset,
            Err(Error::EntryInStblNotFound(_, _, _)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let size = match self.sample_size(sample_id) {
            Ok(size) => size,
            Err(Error::EntryInStblNotFound(_, _, _)) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(Mp4SampleLocation { offset, size }))
    }

    pub(crate) fn read_sample<R: Read + Seek>(
        &self,
        reader: &mut R,
        sample_id: u32,
    ) -> Result<Option<Mp4Sample>> {
        let sample_offset = match self.sample_offset(sample_id) {
            Ok(offset) => offset,
            Err(Error::EntryInStblNotFound(_, _, _)) => return Ok(None),
            Err(err) => return Err(err),
        };
        let sample_size = match self.sample_size(sample_id) {
            Ok(size) => size,
            Err(Error::EntryInStblNotFound(_, _, _)) => return Ok(None),
            Err(err) => return Err(err),
        };

        let mut buffer = vec![0x0u8; sample_size as usize];
        reader.seek(SeekFrom::Start(sample_offset))?;
        reader.read_exact(&mut buffer)?;

        let (start_time, duration) = match self.sample_time(sample_id) {
            Ok(time) => time,
            Err(Error::EntryInStblNotFound(_, _, _)) => return Ok(None),
            Err(err) => return Err(err),
        };
        let rendering_offset = self.sample_rendering_offset(sample_id);
        let is_sync = self.is_sync_sample(sample_id);

        Ok(Some(Mp4Sample {
            start_time,
            duration,
            rendering_offset,
            is_sync,
            bytes: Bytes::from(buffer),
        }))
    }
}

fn hevc_codec_string(sample_entry_name: &str, record: &[u8]) -> Result<String> {
    let profile_tier = *record
        .get(1)
        .ok_or(Error::InvalidData("invalid hvcC configuration record"))?;
    let compatibility = u32::from_be_bytes(
        record
            .get(2..6)
            .ok_or(Error::InvalidData("invalid hvcC configuration record"))?
            .try_into()
            .map_err(|_| Error::InvalidData("invalid hvcC configuration record"))?,
    )
    .reverse_bits();
    let constraints = record
        .get(6..12)
        .ok_or(Error::InvalidData("invalid hvcC configuration record"))?;
    let level = record
        .get(12)
        .ok_or(Error::InvalidData("invalid hvcC configuration record"))?;
    let profile_space = match profile_tier >> 6 {
        0 => "",
        1 => "A",
        2 => "B",
        3 => "C",
        _ => unreachable!(),
    };
    let tier = if profile_tier & 0x20 == 0 { 'L' } else { 'H' };
    let mut codec = format!(
        "{sample_entry_name}.{profile_space}{}.{compatibility:X}.{tier}{level}",
        profile_tier & 0x1f
    );
    let constraint_len = constraints
        .iter()
        .rposition(|value| *value != 0)
        .map_or(1, |index| index + 1);
    for value in &constraints[..constraint_len] {
        use std::fmt::Write as _;
        write!(codec, ".{value:02X}").expect("writing to String cannot fail");
    }
    Ok(codec)
}

const fn media_config_track_type(config: &MediaConfig) -> TrackType {
    match config {
        MediaConfig::AvcConfig(_) | MediaConfig::HevcConfig(_) | MediaConfig::Vp9Config(_) => {
            TrackType::Video
        }
        MediaConfig::AacConfig(_) => TrackType::Audio,
        MediaConfig::TtxtConfig(_) => TrackType::Subtitle,
    }
}

const fn video_dimensions(config: &MediaConfig) -> Option<(u16, u16)> {
    match config {
        MediaConfig::AvcConfig(config) => Some((config.width, config.height)),
        MediaConfig::HevcConfig(config) => Some((config.width, config.height)),
        MediaConfig::Vp9Config(config) => Some((config.width, config.height)),
        MediaConfig::AacConfig(_) | MediaConfig::TtxtConfig(_) => None,
    }
}

fn sample_entry_from_media_config(config: &MediaConfig) -> SampleEntry {
    match config {
        MediaConfig::AvcConfig(config) => SampleEntry::Avc1(Avc1Box::new(config)),
        MediaConfig::HevcConfig(config) => SampleEntry::Hev1(Hev1Box::new(config)),
        MediaConfig::Vp9Config(config) => SampleEntry::Vp09(Vp09Box::new(config)),
        MediaConfig::AacConfig(config) => SampleEntry::Mp4a(Mp4aBox::new(config)),
        MediaConfig::TtxtConfig(_) => SampleEntry::Tx3g(Tx3gBox::default()),
    }
}

// TODO creation_time, modification_time
#[derive(Debug, Default)]
pub struct Mp4TrackWriter {
    trak: TrakBox,

    sample_id: u32,
    fixed_sample_size: u32,
    is_fixed_sample_size: bool,
    chunk_samples: u32,
    chunk_duration: u32,
    chunk_buffer: BytesMut,

    samples_per_chunk: u32,
    duration_per_chunk: u32,
}

impl Mp4TrackWriter {
    pub(crate) fn new(track_id: u32, config: &TrackConfig) -> Result<Self> {
        Self::new_with_sample_descriptions(
            track_id,
            config.track_type,
            config.timescale,
            &config.language,
            std::slice::from_ref(&config.media_conf),
        )
    }

    pub(crate) fn new_with_sample_descriptions(
        track_id: u32,
        track_type: TrackType,
        timescale: u32,
        language: &str,
        sample_descriptions: &[MediaConfig],
    ) -> Result<Self> {
        if sample_descriptions.is_empty() {
            return Err(Error::InvalidData(
                "track requires at least one sample description",
            ));
        }
        if sample_descriptions
            .iter()
            .any(|description| media_config_track_type(description) != track_type)
        {
            return Err(Error::InvalidData(
                "sample description does not match the track type",
            ));
        }
        let mut trak = TrakBox::default();
        trak.tkhd.track_id = track_id;
        trak.mdia.mdhd.timescale = timescale;
        trak.mdia.mdhd.language = language.to_owned();
        trak.mdia.hdlr.handler_type = track_type.into();
        trak.mdia.minf.stbl.co64 = Some(Co64Box::default());
        match track_type {
            TrackType::Video => {
                trak.mdia.minf.vmhd = Some(VmhdBox::default());
                let dimensions = sample_descriptions
                    .iter()
                    .filter_map(video_dimensions)
                    .fold(
                        (0, 0),
                        |(width, height), (candidate_width, candidate_height)| {
                            (width.max(candidate_width), height.max(candidate_height))
                        },
                    );
                trak.tkhd.set_width(dimensions.0);
                trak.tkhd.set_height(dimensions.1);
            }
            TrackType::Audio => trak.mdia.minf.smhd = Some(SmhdBox::default()),
            TrackType::Subtitle => {}
        }
        trak.mdia.minf.stbl.stsd.entries = sample_descriptions
            .iter()
            .map(sample_entry_from_media_config)
            .collect();
        Ok(Self {
            trak,
            chunk_buffer: BytesMut::new(),
            sample_id: 1,
            duration_per_chunk: timescale, // 1 second
            ..Self::default()
        })
    }

    fn update_sample_sizes(&mut self, size: u32) {
        if self.trak.mdia.minf.stbl.stsz.sample_count == 0 {
            if size == 0 {
                self.trak.mdia.minf.stbl.stsz.sample_size = 0;
                self.is_fixed_sample_size = false;
                self.trak.mdia.minf.stbl.stsz.sample_sizes.push(0);
            } else {
                self.trak.mdia.minf.stbl.stsz.sample_size = size;
                self.fixed_sample_size = size;
                self.is_fixed_sample_size = true;
            }
        } else if self.is_fixed_sample_size {
            if self.fixed_sample_size != size {
                self.is_fixed_sample_size = false;
                if self.trak.mdia.minf.stbl.stsz.sample_size > 0 {
                    self.trak.mdia.minf.stbl.stsz.sample_size = 0;
                    for _ in 0..self.trak.mdia.minf.stbl.stsz.sample_count {
                        self.trak
                            .mdia
                            .minf
                            .stbl
                            .stsz
                            .sample_sizes
                            .push(self.fixed_sample_size);
                    }
                }
                self.trak.mdia.minf.stbl.stsz.sample_sizes.push(size);
            }
        } else {
            self.trak.mdia.minf.stbl.stsz.sample_sizes.push(size);
        }
        self.trak.mdia.minf.stbl.stsz.sample_count += 1;
    }

    fn update_sample_times(&mut self, dur: u32) {
        if let Some(ref mut entry) = self.trak.mdia.minf.stbl.stts.entries.last_mut()
            && entry.sample_delta == dur
        {
            entry.sample_count += 1;
            return;
        }

        let entry = SttsEntry {
            sample_count: 1,
            sample_delta: dur,
        };
        self.trak.mdia.minf.stbl.stts.entries.push(entry);
    }

    fn update_rendering_offsets(&mut self, offset: i32) {
        let ctts = if let Some(ref mut ctts) = self.trak.mdia.minf.stbl.ctts {
            ctts
        } else {
            if offset == 0 {
                return;
            }
            let mut ctts = CttsBox::default();
            if self.sample_id > 1 {
                let entry = CttsEntry {
                    sample_count: self.sample_id - 1,
                    sample_offset: 0,
                };
                ctts.entries.push(entry);
            }
            self.trak.mdia.minf.stbl.ctts = Some(ctts);
            self.trak.mdia.minf.stbl.ctts.as_mut().unwrap()
        };

        if let Some(ref mut entry) = ctts.entries.last_mut()
            && entry.sample_offset == offset
        {
            entry.sample_count += 1;
            return;
        }

        let entry = CttsEntry {
            sample_count: 1,
            sample_offset: offset,
        };
        ctts.entries.push(entry);
    }

    fn update_sync_samples(&mut self, is_sync: bool) {
        if !is_sync {
            return;
        }

        if let Some(ref mut stss) = self.trak.mdia.minf.stbl.stss {
            stss.entries.push(self.sample_id);
        } else {
            // Create the stts box if not found and push the entry.
            let mut stss = StssBox::default();
            stss.entries.push(self.sample_id);
            self.trak.mdia.minf.stbl.stss = Some(stss);
        };
    }

    const fn is_chunk_full(&self) -> bool {
        if self.samples_per_chunk > 0 {
            self.chunk_samples >= self.samples_per_chunk
        } else {
            self.chunk_duration >= self.duration_per_chunk
        }
    }

    const fn update_durations(&mut self, dur: u32, movie_timescale: u32) {
        self.trak.mdia.mdhd.duration += dur as u64;
        if self.trak.mdia.mdhd.duration > (u32::MAX as u64) {
            self.trak.mdia.mdhd.version = 1;
        }
        self.trak.tkhd.duration +=
            dur as u64 * movie_timescale as u64 / self.trak.mdia.mdhd.timescale as u64;
        if self.trak.tkhd.duration > (u32::MAX as u64) {
            self.trak.tkhd.version = 1;
        }
    }

    pub(crate) fn write_sample<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        sample: &Mp4Sample,
        movie_timescale: u32,
    ) -> Result<u64> {
        self.chunk_buffer.extend_from_slice(&sample.bytes);
        self.chunk_samples += 1;
        self.chunk_duration += sample.duration;
        self.update_sample_sizes(sample.bytes.len() as u32);
        self.update_sample_times(sample.duration);
        self.update_rendering_offsets(sample.rendering_offset);
        self.update_sync_samples(sample.is_sync);
        if self.is_chunk_full() {
            self.write_chunk(writer)?;
        }
        self.update_durations(sample.duration, movie_timescale);

        self.sample_id += 1;

        Ok(self.trak.tkhd.duration)
    }

    const fn chunk_count(&self) -> u32 {
        let co64 = self.trak.mdia.minf.stbl.co64.as_ref().unwrap();
        co64.entries.len() as u32
    }

    fn update_sample_to_chunk(&mut self, chunk_id: u32) {
        if let Some(entry) = self.trak.mdia.minf.stbl.stsc.entries.last()
            && entry.samples_per_chunk == self.chunk_samples
        {
            return;
        }

        let entry = StscEntry {
            first_chunk: chunk_id,
            samples_per_chunk: self.chunk_samples,
            sample_description_index: 1,
            first_sample: self.sample_id - self.chunk_samples + 1,
        };
        self.trak.mdia.minf.stbl.stsc.entries.push(entry);
    }

    fn update_chunk_offsets(&mut self, offset: u64) {
        let co64 = self.trak.mdia.minf.stbl.co64.as_mut().unwrap();
        co64.entries.push(offset);
    }

    fn write_chunk<W: Write + Seek>(&mut self, writer: &mut W) -> Result<()> {
        if self.chunk_buffer.is_empty() {
            return Ok(());
        }
        let chunk_offset = writer.stream_position()?;

        writer.write_all(&self.chunk_buffer)?;

        self.update_sample_to_chunk(self.chunk_count() + 1);
        self.update_chunk_offsets(chunk_offset);

        self.chunk_buffer.clear();
        self.chunk_samples = 0;
        self.chunk_duration = 0;

        Ok(())
    }

    fn max_sample_size(&self) -> u32 {
        if self.trak.mdia.minf.stbl.stsz.sample_size > 0 {
            self.trak.mdia.minf.stbl.stsz.sample_size
        } else {
            let mut max_size = 0;
            for sample_size in self.trak.mdia.minf.stbl.stsz.sample_sizes.iter() {
                max_size = cmp::max(max_size, *sample_size);
            }
            max_size
        }
    }

    pub(crate) fn write_end<W: Write + Seek>(&mut self, writer: &mut W) -> Result<TrakBox> {
        self.write_chunk(writer)?;

        let max_sample_size = self.max_sample_size();
        if let Some(mp4a) = self.trak.mdia.minf.stbl.stsd.mp4a_mut()
            && let Some(ref mut esds) = mp4a.esds
        {
            esds.es_desc.dec_config.buffer_size_db = max_sample_size;
            // TODO
            // mp4a.esds.es_desc.dec_config.max_bitrate
            // mp4a.esds.es_desc.dec_config.avg_bitrate
        }
        if let Ok(stco) = StcoBox::try_from(self.trak.mdia.minf.stbl.co64.as_ref().unwrap()) {
            self.trak.mdia.minf.stbl.stco = Some(stco);
            self.trak.mdia.minf.stbl.co64 = None;
        }

        Ok(self.trak.clone())
    }
}

//! Read-only reports derived from the embedded camera catalog.

use crate::camera_database::CameraDatabase;

const COMMON_ONVIF_PROBE_PORTS: &[u16] = &[
    80, 443, 554, 2020, 5000, 8000, 8080, 8443, 8554, 8899, 10080,
];

/// A frequency observed in explicit camera-catalog ONVIF service-port fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnvifPortFrequency {
    port: u16,
    camera_count: usize,
}

impl OnvifPortFrequency {
    pub(crate) const fn new(port: u16, camera_count: usize) -> Self {
        Self { port, camera_count }
    }

    /// Returns the explicitly declared ONVIF service port.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Returns how many ONVIF-capable catalog cameras declare this port.
    pub const fn camera_count(&self) -> usize {
        self.camera_count
    }
}

/// A report of ONVIF capability and service-port evidence in the camera catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnvifPortReport {
    camera_count: usize,
    onvif_capable_camera_count: usize,
    catalog_port_frequencies: Box<[OnvifPortFrequency]>,
}

impl OnvifPortReport {
    pub(crate) const fn new(
        camera_count: usize,
        onvif_capable_camera_count: usize,
        catalog_port_frequencies: Box<[OnvifPortFrequency]>,
    ) -> Self {
        Self {
            camera_count,
            onvif_capable_camera_count,
            catalog_port_frequencies,
        }
    }

    /// Returns the number of cameras parsed from matching catalog JSON and CSV records.
    pub const fn camera_count(&self) -> usize {
        self.camera_count
    }

    /// Returns the number of catalog cameras that advertise ONVIF support.
    pub const fn onvif_capable_camera_count(&self) -> usize {
        self.onvif_capable_camera_count
    }

    /// Returns only ports explicitly declared by upstream catalog records, ordered by frequency.
    pub const fn catalog_port_frequencies(&self) -> &[OnvifPortFrequency] {
        &self.catalog_port_frequencies
    }

    /// Reports whether upstream catalog records include any explicit ONVIF service ports.
    pub const fn has_catalog_port_evidence(&self) -> bool {
        !self.catalog_port_frequencies.is_empty()
    }
}

/// Loads the embedded catalog and reports its explicit ONVIF port evidence.
///
/// # Errors
///
/// Returns an error when the embedded catalog fails integrity validation or cannot be parsed.
pub fn onvif_port_report() -> anyhow::Result<OnvifPortReport> {
    Ok(CameraDatabase::load_embedded()?.onvif_port_report())
}

/// Returns curated fallback ports for runtime ONVIF probing.
///
/// These values are not derived from the camera catalog and must not be persisted until a camera
/// responds on the selected port.
pub const fn common_onvif_probe_ports() -> &'static [u16] {
    COMMON_ONVIF_PROBE_PORTS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_probe_ports_are_unique_and_ascending() {
        assert!(
            common_onvif_probe_ports()
                .windows(2)
                .all(|ports| ports[0] < ports[1])
        );
    }
}

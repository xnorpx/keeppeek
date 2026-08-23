//! Public test fixtures for exercising KeepPeek integrations.

use crate::camera_database::CameraDatabase;

/// One fictional camera entry for an in-memory test catalog.
#[derive(Debug, Clone)]
pub struct TestCatalogCamera {
    pub(crate) id: String,
    pub(crate) brand: String,
    pub(crate) model: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) main_rtsp_template: Option<String>,
    pub(crate) sub_rtsp_template: Option<String>,
}

impl TestCatalogCamera {
    /// Creates a fictional camera entry with a stable catalog identity.
    pub fn new(id: impl Into<String>, brand: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            brand: brand.into(),
            model: model.into(),
            aliases: Vec::new(),
            main_rtsp_template: None,
            sub_rtsp_template: None,
        }
    }

    /// Adds a model alias that resolves to this camera.
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    /// Adds credential-free main and substream templates using the `{ip}` placeholder.
    #[must_use]
    pub fn with_stream_templates(
        mut self,
        main: impl Into<String>,
        sub: impl Into<String>,
    ) -> Self {
        self.main_rtsp_template = Some(main.into());
        self.sub_rtsp_template = Some(sub.into());
        self
    }
}

/// An immutable catalog made from fictional cameras for integration tests.
#[derive(Debug)]
pub struct TestCameraCatalog {
    database: CameraDatabase,
}

impl TestCameraCatalog {
    /// Builds a test catalog and validates each fixture identity before it is injected.
    ///
    /// # Errors
    ///
    /// Returns an error when a camera ID, manufacturer, or model is empty, or when IDs repeat.
    pub fn new(cameras: impl IntoIterator<Item = TestCatalogCamera>) -> anyhow::Result<Self> {
        Ok(Self {
            database: CameraDatabase::from_test_cameras(cameras)?,
        })
    }

    /// Returns camera entries matching the device identities emitted by `test-camera`.
    pub fn standard() -> Self {
        Self::new([
            TestCatalogCamera::new("keeppeek-test-rtsp", "Test Camera", "RTSP Test Camera")
                .with_stream_templates("rtsp://{ip}/main", "rtsp://{ip}/sub"),
            TestCatalogCamera::new("keeppeek-test-reolink", "Reolink", "RLC-Test")
                .with_stream_templates("rtsp://{ip}/main", "rtsp://{ip}/sub"),
            TestCatalogCamera::new("keeppeek-test-battery", "Reolink", "Argus-Test")
                .with_stream_templates("rtsp://{ip}/main", "rtsp://{ip}/sub"),
        ])
        .expect("built-in test camera catalog must be valid")
    }

    pub(crate) fn into_database(self) -> CameraDatabase {
        self.database
    }
}

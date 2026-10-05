use crate::storage::{catalog::RecordingCatalogHandle, volumes::runtime::ReservedFile};
use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};

pub(super) enum WriterFile {
    Legacy(File),
    Named(Box<ReservedFile>),
}

impl WriterFile {
    pub(super) fn complete(
        self,
        catalog: Option<&RecordingCatalogHandle>,
        id: &str,
        active: &Path,
        destination: &Path,
    ) -> io::Result<()> {
        match self {
            Self::Legacy(file) => {
                drop(file);
                std::fs::rename(active, destination)?;
                if let Some(catalog) = catalog {
                    catalog
                        .update_recording_path(id, destination, true)
                        .map_err(io::Error::other)?;
                }
            }
            Self::Named(mut file) => {
                let evidence = file.evidence().map_err(io::Error::other)?;
                file.finalize(evidence).map_err(io::Error::other)?;
            }
        }
        Ok(())
    }
}

impl Write for WriterFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Legacy(file) => file.write(bytes),
            Self::Named(file) => file.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Legacy(file) => file.flush(),
            Self::Named(file) => file.flush(),
        }
    }
}

impl Seek for WriterFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Legacy(file) => file.seek(position),
            Self::Named(file) => file.seek(position),
        }
    }
}

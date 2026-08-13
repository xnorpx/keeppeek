use crate::mp4box::BoxType;

#[derive(Debug)]
pub enum Error {
    IoError(std::io::Error),
    InvalidData(&'static str),
    BoxNotFound(BoxType),
    Box2NotFound(BoxType, BoxType),
    TrakNotFound(u32),
    BoxInTrakNotFound(u32, BoxType),
    BoxInTrafNotFound(u32, BoxType),
    BoxInStblNotFound(u32, BoxType),
    EntryInStblNotFound(u32, BoxType, u32),
    EntryInTrunNotFound(u32, BoxType, u32),
    UnsupportedBoxVersion(BoxType, u8),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(err) => write!(f, "{err}"),
            Self::InvalidData(msg) => write!(f, "{msg}"),
            Self::BoxNotFound(box_type) => write!(f, "{box_type} not found"),
            Self::Box2NotFound(box1, box2) => write!(f, "{box1} and {box2} not found"),
            Self::TrakNotFound(track_id) => write!(f, "trak[{track_id}] not found"),
            Self::BoxInTrakNotFound(track_id, box_type) => {
                write!(f, "trak[{track_id}].{box_type} not found")
            }
            Self::BoxInTrafNotFound(track_id, box_type) => {
                write!(f, "traf[{track_id}].{box_type} not found")
            }
            Self::BoxInStblNotFound(track_id, box_type) => {
                write!(f, "trak[{track_id}].stbl.{box_type} not found")
            }
            Self::EntryInStblNotFound(track_id, box_type, entry_id) => write!(
                f,
                "trak[{track_id}].stbl.{box_type}.entry[{entry_id}] not found"
            ),
            Self::EntryInTrunNotFound(track_id, box_type, entry_id) => write!(
                f,
                "traf[{track_id}].trun.{box_type}.entry[{entry_id}] not found"
            ),
            Self::UnsupportedBoxVersion(box_type, version) => {
                write!(f, "{box_type} version {version} is not supported")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoError(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::IoError(err)
    }
}

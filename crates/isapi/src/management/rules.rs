use super::{Query, boolean, nonzero, replacement};
use crate::error::Kind;
use crate::{Document, Error, Request};

/// Supported smart-rule configuration families.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleKind {
    /// Line-crossing detection.
    Line,
    /// Field intrusion detection.
    Field,
    /// Entry into a configured region.
    RegionEntrance,
    /// Exit from a configured region.
    RegionExit,
}

impl RuleKind {
    pub(super) const fn resource(self) -> &'static str {
        match self {
            Self::Line => "LineDetection",
            Self::Field => "FieldDetection",
            Self::RegionEntrance => "RegionEntrance",
            Self::RegionExit => "RegionExiting",
        }
    }
}

/// A smart-rule document preserving line/region geometry and vendor extensions.
#[derive(Clone, Debug)]
pub struct Rule {
    kind: RuleKind,
    document: Document,
    enabled: bool,
}

impl Rule {
    /// Queries the chosen smart-rule family for a video channel.
    ///
    /// # Errors
    /// Rejects channel zero.
    pub fn query(kind: RuleKind, channel: u32) -> Result<Query<Self>, Error> {
        let parse = match kind {
            RuleKind::Line => |document| Self::parse(RuleKind::Line, document),
            RuleKind::Field => |document| Self::parse(RuleKind::Field, document),
            RuleKind::RegionEntrance => |document| Self::parse(RuleKind::RegionEntrance, document),
            RuleKind::RegionExit => |document| Self::parse(RuleKind::RegionExit, document),
        };
        Query::new(
            format!("/ISAPI/Smart/{}/{}", kind.resource(), nonzero(channel)?),
            parse,
        )
    }
    fn parse(kind: RuleKind, document: Document) -> Result<Self, Error> {
        let root = document.root(kind.resource())?;
        Ok(Self {
            kind,
            enabled: boolean(root, "enabled")?,
            document,
        })
    }
    /// Returns whether the selected rule family is enabled.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }
    /// Updates only the enabled field without replacing region geometry.
    ///
    /// # Errors
    /// Rejects an inconsistent stored document.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), Error> {
        self.document
            .set(self.kind.resource(), &["enabled"], enabled.into())?;
        self.enabled = enabled;
        Ok(())
    }
    /// Builds a replacement request; it does not transmit the change.
    ///
    /// # Errors
    /// Rejects channel zero or an oversized body.
    pub fn update(&self, channel: u32) -> Result<Request, Error> {
        if channel == 0 {
            return Err(Error::new(Kind::InvalidInput));
        }
        replacement(
            &self.document,
            Self::query(self.kind, channel)?
                .request
                .resource()
                .to_owned(),
        )
    }
    /// Returns the complete validated configuration document.
    pub const fn document(&self) -> &Document {
        &self.document
    }
}

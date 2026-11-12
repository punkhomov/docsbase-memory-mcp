//! Encode source paths using exactly the lexical components used by Path Eq/Ord.
//! No filesystem canonicalization: preserve `..` and verbatim-prefix semantics.
use std::ffi::OsStr;
use std::path::{Component, Path, Prefix};

use serde::ser::{Error, SerializeSeq};
use serde::{Serialize, Serializer};

use crate::config::schema::SourceId;

pub(super) fn source<S: Serializer>(source: &SourceId, serializer: S) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct FileSource<'a> {
        kind: &'static str,
        value: CanonicalPath<'a>,
    }
    match source {
        SourceId::Global(path) => FileSource {
            kind: "global",
            value: CanonicalPath(path),
        }
        .serialize(serializer),
        SourceId::Project(path) => FileSource {
            kind: "project",
            value: CanonicalPath(path),
        }
        .serialize(serializer),
        _ => source.serialize(serializer),
    }
}
struct CanonicalPath<'a>(&'a Path);
impl Serialize for CanonicalPath<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        for component in self.0.components() {
            let part = part(component).map_err(S::Error::custom)?;
            seq.serialize_element(&part)?;
        }
        seq.end()
    }
}
#[derive(Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum Part<'a> {
    Prefix(PrefixPart<'a>),
    RootDir,
    CurDir,
    ParentDir,
    Normal(&'a str),
}
#[derive(Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum PrefixPart<'a> {
    Verbatim(&'a str),
    VerbatimUnc(&'a str, &'a str),
    VerbatimDisk(u8),
    DeviceNs(&'a str),
    Unc(&'a str, &'a str),
    Disk(u8),
}
fn text(value: &OsStr) -> Result<&str, &'static str> {
    value.to_str().ok_or("source path is not UTF-8")
}
fn part(component: Component<'_>) -> Result<Part<'_>, &'static str> {
    Ok(match component {
        Component::Prefix(prefix) => Part::Prefix(match prefix.kind() {
            Prefix::Verbatim(value) => PrefixPart::Verbatim(text(value)?),
            Prefix::VerbatimUNC(server, share) => {
                PrefixPart::VerbatimUnc(text(server)?, text(share)?)
            }
            Prefix::VerbatimDisk(disk) => PrefixPart::VerbatimDisk(disk),
            Prefix::DeviceNS(value) => PrefixPart::DeviceNs(text(value)?),
            Prefix::UNC(server, share) => PrefixPart::Unc(text(server)?, text(share)?),
            Prefix::Disk(disk) => PrefixPart::Disk(disk),
        }),
        Component::RootDir => Part::RootDir,
        Component::CurDir => Part::CurDir,
        Component::ParentDir => Part::ParentDir,
        Component::Normal(value) => Part::Normal(text(value)?),
    })
}

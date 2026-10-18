use std::{collections::BTreeMap, net::IpAddr};

#[derive(Debug, Default)]
pub struct CameraProfileIds {
    by_ip: BTreeMap<IpAddr, String>,
    by_id: BTreeMap<String, String>,
}

impl CameraProfileIds {
    pub(crate) fn new(by_ip: BTreeMap<IpAddr, String>) -> anyhow::Result<Self> {
        let mut by_id = BTreeMap::new();
        for (ip, identity) in &by_ip {
            anyhow::ensure!(
                by_id.insert(identity.clone(), ip.to_string()).is_none(),
                "camera profile identity is assigned to multiple sources"
            );
        }
        Ok(Self { by_ip, by_id })
    }

    pub(crate) fn camera_id(&self, ip: IpAddr) -> String {
        self.by_ip
            .get(&ip)
            .cloned()
            .unwrap_or_else(|| ip.to_string())
    }

    pub(crate) fn source(&self, identity: &str) -> Option<IpAddr> {
        self.source_id(identity)?.parse().ok()
    }

    pub(crate) fn source_id<'a>(&'a self, identity: &'a str) -> Option<&'a str> {
        self.by_id.get(identity).map(String::as_str).or_else(|| {
            identity
                .parse::<IpAddr>()
                .ok()
                .filter(|ip| !self.by_ip.contains_key(ip))
                .map(|_| identity)
        })
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&String, &String)> {
        self.by_id.iter()
    }

    pub(crate) fn privacy_overrides(
        &self,
        profiles: &crate::profiles::ProfileRegistry,
    ) -> BTreeMap<String, bool> {
        profiles
            .active_privacy_overrides()
            .into_iter()
            .filter_map(|(identity, enabled)| {
                self.source_id(&identity)
                    .map(|source| (source.to_owned(), enabled))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_identity_index_records_owned_string_capacity_for_127_sources() {
        let identities = (1..=127)
            .map(|index| {
                (
                    format!("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ff{index:02x}")
                        .parse()
                        .unwrap(),
                    format!("00000000-0000-4000-8000-{index:012x}"),
                )
            })
            .collect();
        let index = CameraProfileIds::new(identities).unwrap();
        assert_eq!(index.entries().count(), 127);
        for (identity, source) in index.entries() {
            assert_eq!(index.camera_id(source.parse().unwrap()), *identity);
            assert_eq!(index.source_id(identity), Some(source.as_str()));
        }
        let string_capacity_bytes = index.by_ip.values().map(String::capacity).sum::<usize>()
            + index
                .by_id
                .iter()
                .map(|(identity, source)| identity.capacity() + source.capacity())
                .sum::<usize>();
        eprintln!(
            "127-source identity index: header_bytes={} owned_string_capacity_bytes={string_capacity_bytes}; BTree node allocations excluded",
            std::mem::size_of::<CameraProfileIds>()
        );
    }
}

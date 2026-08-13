use ipnet::Ipv4Net;
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use std::{collections::BTreeSet, net::Ipv4Addr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalNetwork {
    pub(super) interface_name: String,
    pub(super) interface_ip: Ipv4Addr,
    pub(super) network: Ipv4Net,
    pub(super) broadcast: Ipv4Addr,
}

pub(super) fn local_networks() -> anyhow::Result<Vec<LocalNetwork>> {
    let interfaces = NetworkInterface::show()?;
    Ok(networks_from_interfaces(&interfaces))
}

pub(super) fn scan_networks(extra_subnets: &[u8]) -> anyhow::Result<Vec<LocalNetwork>> {
    let mut networks = local_networks()?;
    let prefixes = networks
        .iter()
        .map(|network| {
            let octets = network.interface_ip.octets();
            (octets[0], octets[1])
        })
        .collect::<BTreeSet<_>>();

    for (first, second) in prefixes {
        for &third in extra_subnets {
            let address = Ipv4Addr::new(first, second, third, 0);
            let network = Ipv4Net::new(address, 24)?;
            if networks.iter().any(|local| local.network == network) {
                continue;
            }
            networks.push(LocalNetwork {
                interface_name: "legacy-subnet".to_string(),
                interface_ip: address,
                network,
                broadcast: network.broadcast(),
            });
        }
    }

    Ok(networks)
}

fn networks_from_interfaces(interfaces: &[NetworkInterface]) -> Vec<LocalNetwork> {
    let mut networks = interfaces
        .iter()
        .filter(|interface| !interface.internal)
        .flat_map(|interface| {
            interface.addr.iter().filter_map(|address| {
                let Addr::V4(address) = address else {
                    return None;
                };
                if address.ip.is_unspecified() || address.ip.is_loopback() {
                    return None;
                }

                let netmask = address.netmask?;
                let prefix_len = netmask_to_prefix(netmask)?;
                let network = Ipv4Net::new(address.ip, prefix_len).ok()?;

                Some(LocalNetwork {
                    interface_name: interface.name.clone(),
                    interface_ip: address.ip,
                    network,
                    broadcast: address.broadcast.unwrap_or_else(|| network.broadcast()),
                })
            })
        })
        .collect::<Vec<_>>();

    networks.sort_by_key(|network| {
        (
            u32::from(network.network.network()),
            network.network.prefix_len(),
            u32::from(network.interface_ip),
            network.interface_name.clone(),
        )
    });
    networks.dedup_by(|left, right| {
        left.interface_ip == right.interface_ip && left.network == right.network
    });
    networks
}

pub(super) fn scan_targets(networks: &[LocalNetwork]) -> Vec<Ipv4Addr> {
    let own_addresses = networks
        .iter()
        .map(|network| network.interface_ip)
        .collect::<BTreeSet<_>>();
    let mut targets = BTreeSet::new();

    for local in networks {
        for address in local.network.hosts() {
            if !own_addresses.contains(&address) {
                targets.insert(address);
            }
        }
    }

    targets.into_iter().collect()
}

fn netmask_to_prefix(netmask: Ipv4Addr) -> Option<u8> {
    let bits = u32::from(netmask);
    let prefix_len = bits.leading_ones() as u8;
    let expected = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len)
    };
    (bits == expected).then_some(prefix_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_network(ip: Ipv4Addr, prefix_len: u8) -> LocalNetwork {
        let network = Ipv4Net::new(ip, prefix_len).expect("test CIDR must be valid");
        LocalNetwork {
            interface_name: "test0".to_string(),
            interface_ip: ip,
            network,
            broadcast: network.broadcast(),
        }
    }

    #[test]
    fn scan_targets_use_actual_prefix_and_exclude_own_address() {
        let networks = [local_network(Ipv4Addr::new(192, 168, 4, 10), 30)];

        assert_eq!(scan_targets(&networks), vec![Ipv4Addr::new(192, 168, 4, 9)]);
    }

    #[test]
    fn scan_targets_deduplicate_overlapping_networks() {
        let networks = [
            local_network(Ipv4Addr::new(10, 0, 0, 1), 30),
            local_network(Ipv4Addr::new(10, 0, 0, 2), 29),
        ];

        assert_eq!(
            scan_targets(&networks),
            vec![
                Ipv4Addr::new(10, 0, 0, 3),
                Ipv4Addr::new(10, 0, 0, 4),
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(10, 0, 0, 6),
            ]
        );
    }

    #[test]
    fn netmask_rejects_non_contiguous_bits() {
        assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 255, 254, 0)), Some(23));
        assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 0, 255, 0)), None);
    }
}

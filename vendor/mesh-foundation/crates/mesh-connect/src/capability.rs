//! Bounded application protocol version negotiation.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "signed-discovery",
    derive(serde::Serialize, serde::Deserialize)
)]
pub struct Capability {
    pub protocol: [u8; 32],
    pub minimum: u16,
    pub maximum: u16,
}
pub fn valid(items: &[Capability]) -> bool {
    items.len() <= 32
        && items.iter().enumerate().all(|(i, c)| {
            c.minimum > 0
                && c.minimum <= c.maximum
                && !items[..i].iter().any(|p| p.protocol == c.protocol)
        })
}
pub fn negotiate(local: &[Capability], remote: &[Capability], protocol: [u8; 32]) -> Option<u16> {
    if !valid(local) || !valid(remote) {
        return None;
    }
    let a = local.iter().find(|c| c.protocol == protocol)?;
    let b = remote.iter().find(|c| c.protocol == protocol)?;
    let upper = a.maximum.min(b.maximum);
    (upper >= a.minimum.max(b.minimum)).then_some(upper)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intersects_and_rejects_duplicates() {
        let a = Capability {
            protocol: [1; 32],
            minimum: 1,
            maximum: 4,
        };
        let b = Capability {
            minimum: 3,
            maximum: 6,
            ..a.clone()
        };
        assert_eq!(negotiate(std::slice::from_ref(&a), &[b], [1; 32]), Some(4));
        assert!(!valid(&[a.clone(), a]));
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn rejects_disjoint_versions_unknown_protocols_and_unbounded_advertisements() {
        let local = [Capability {
            protocol: [1; 32],
            minimum: 1,
            maximum: 2,
        }];
        let remote = [Capability {
            protocol: [1; 32],
            minimum: 3,
            maximum: 4,
        }];
        assert_eq!(negotiate(&local, &remote, [1; 32]), None);
        assert_eq!(negotiate(&local, &local, [2; 32]), None);
        assert!(!valid(&vec![local[0].clone(); 33]));
        assert!(!valid(&[Capability {
            protocol: [1; 32],
            minimum: 0,
            maximum: 1
        }]));
    }
}

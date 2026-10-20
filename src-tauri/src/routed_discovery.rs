//! Desktop inventories share the core's provider-neutral discovery path.

use mesh_talk_core::discovery::candidates::CandidateSource;
use mesh_talk_core::transport::net::POST_OFFICE_DISCOVERY_PORT;
use std::collections::HashSet;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

struct DesktopCandidates {
    sources: [Arc<dyn CandidateSource>; 2],
    deadline: Duration,
}

pub(crate) fn desktop_candidates(discovery_port: u16) -> Arc<dyn CandidateSource> {
    Arc::new(DesktopCandidates {
        sources: [
            Arc::new(crate::tailscale_discovery::TailscaleCandidates { discovery_port }),
            Arc::new(crate::easytier_discovery::EasyTierCandidates { discovery_port }),
        ],
        // The core bounds the combined snapshot at three seconds.
        deadline: Duration::from_millis(2500),
    })
}

impl CandidateSource for DesktopCandidates {
    fn snapshot(&self) -> Pin<Box<dyn Future<Output = Vec<SocketAddr>> + Send + '_>> {
        Box::pin(async move {
            let (first, second) = tokio::join!(
                tokio::time::timeout(self.deadline, self.sources[0].snapshot()),
                tokio::time::timeout(self.deadline, self.sources[1].snapshot()),
            );
            // VPN inventories contain hosts, not application service ports.
            // Expand each host before interleaving the providers so both
            // desktop and the standard installed post office get a turn.
            let endpoints = |hosts: Vec<SocketAddr>| {
                hosts.into_iter().flat_map(|desktop| {
                    let mut post_office = desktop;
                    post_office.set_port(POST_OFFICE_DISCOVERY_PORT);
                    [desktop, post_office]
                })
            };
            let mut providers = [
                endpoints(first.unwrap_or_default()),
                endpoints(second.unwrap_or_default()),
            ];
            let mut seen = HashSet::new();
            let mut candidates = Vec::new();
            // Interleave so one full provider cannot starve the other at the
            // shared 256-address budget. Identity deduplication stays in core.
            loop {
                let mut advanced = false;
                for provider in &mut providers {
                    if let Some(endpoint) = provider.next() {
                        advanced = true;
                        if seen.insert(endpoint) {
                            candidates.push(endpoint);
                            if candidates.len() == 256 {
                                return candidates;
                            }
                        }
                    }
                }
                if !advanced {
                    return candidates;
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "../tests/support/easytier_live.rs"]
mod live_tests;

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<SocketAddr>);
    impl CandidateSource for Fixed {
        fn snapshot(&self) -> Pin<Box<dyn Future<Output = Vec<SocketAddr>> + Send + '_>> {
            Box::pin(async { self.0.clone() })
        }
    }

    struct Hanging;
    impl CandidateSource for Hanging {
        fn snapshot(&self) -> Pin<Box<dyn Future<Output = Vec<SocketAddr>> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn inventories_probe_desktop_and_dedicated_post_office_on_the_same_host() {
        let source = DesktopCandidates {
            sources: [
                Arc::new(Fixed(vec!["100.64.0.10:47474".parse().unwrap()])),
                Arc::new(Fixed(Vec::new())),
            ],
            deadline: Duration::from_millis(30),
        };
        let result = source.snapshot().await;
        assert!(result.contains(&"100.64.0.10:47474".parse().unwrap()));
        assert!(result.contains(&"100.64.0.10:47478".parse().unwrap()));
    }

    #[tokio::test]
    async fn ipv6_custom_and_already_dedicated_endpoints_preserve_scope_and_deduplicate() {
        let desktop: SocketAddr = "[fe80::1%7]:49000".parse().unwrap();
        let mut relay = desktop;
        relay.set_port(47478);
        let source = DesktopCandidates {
            sources: [
                Arc::new(Fixed(vec![desktop, relay])),
                Arc::new(Fixed(vec![relay])),
            ],
            deadline: Duration::from_millis(30),
        };
        assert_eq!(source.snapshot().await, vec![desktop, relay]);
    }

    #[tokio::test]
    async fn merges_both_providers_without_duplicates_or_starvation() {
        let first: Vec<_> = (1..=256)
            .map(|i| SocketAddr::from(([10, 0, (i / 256) as u8, (i % 256) as u8], 47474)))
            .collect();
        let second: Vec<_> = (1..=256)
            .map(|i| SocketAddr::from(([10, 1, (i / 256) as u8, (i % 256) as u8], 47474)))
            .collect();
        let source = DesktopCandidates {
            sources: [
                Arc::new(Fixed(first.clone())),
                Arc::new(Fixed(second.clone())),
            ],
            deadline: Duration::from_millis(30),
        };
        let result = source.snapshot().await;
        assert_eq!(result.len(), 256);
        assert!(result.contains(&first[63]));
        assert!(result.contains(&second[63]));
        for host in [first[63], second[63]] {
            assert!(result.contains(&SocketAddr::new(host.ip(), 47478)));
        }
        assert_eq!(result.iter().copied().collect::<HashSet<_>>().len(), 256);
        let same = first[0];
        let source = DesktopCandidates {
            sources: [
                Arc::new(Fixed(vec![same])),
                Arc::new(Fixed(vec![same, second[0]])),
            ],
            deadline: Duration::from_millis(30),
        };
        assert_eq!(
            source.snapshot().await,
            vec![
                same,
                SocketAddr::new(same.ip(), 47478),
                second[0],
                SocketAddr::new(second[0].ip(), 47478),
            ]
        );
    }

    #[tokio::test]
    async fn a_hung_provider_cannot_discard_the_other_in_either_order() {
        let endpoint = "10.1.2.3:47474".parse().unwrap();
        for sources in [
            [
                Arc::new(Hanging) as Arc<dyn CandidateSource>,
                Arc::new(Fixed(vec![endpoint])),
            ],
            [
                Arc::new(Fixed(vec![endpoint])) as Arc<dyn CandidateSource>,
                Arc::new(Hanging),
            ],
        ] {
            let source = DesktopCandidates {
                sources,
                deadline: Duration::from_millis(30),
            };
            assert_eq!(
                source.snapshot().await,
                vec![endpoint, SocketAddr::new(endpoint.ip(), 47478)]
            );
        }
    }
}

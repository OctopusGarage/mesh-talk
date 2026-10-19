//! Desktop inventories share the core's provider-neutral discovery path.

use mesh_talk_core::discovery::candidates::CandidateSource;
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
            let mut providers = [
                first.unwrap_or_default().into_iter(),
                second.unwrap_or_default().into_iter(),
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
        assert!(result.contains(&first[127]));
        assert!(result.contains(&second[127]));
        let same = first[0];
        let source = DesktopCandidates {
            sources: [
                Arc::new(Fixed(vec![same])),
                Arc::new(Fixed(vec![same, second[0]])),
            ],
            deadline: Duration::from_millis(30),
        };
        assert_eq!(source.snapshot().await, vec![same, second[0]]);
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
            assert_eq!(source.snapshot().await, vec![endpoint]);
        }
    }
}

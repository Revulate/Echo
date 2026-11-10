//! Hooks for the isolated link-preview acceptance matrix. Each is inert unless
//! that fixture is enabled.

use super::*;

impl EmbedCache {
    /// Install one synthetic, terminal preview for the isolated native
    /// link-preview acceptance matrix. This deliberately bypasses resolver
    /// work: the matrix is about disclosure/rendering policy, and must not
    /// depend on a remote site or create a metadata request.
    pub fn install_benchmark_fixture(&self, metadata: EmbedMetadata) -> bool {
        if !crate::bench::ui_fixture::link_preview_matrix_enabled() {
            return false;
        }

        let resource_key =
            crate::backend::link_types::classify_link(&metadata.link_info.url).resource_key;
        self.entries.lock_or_recover().put(
            resource_key.0,
            CachedEmbed {
                metadata,
                cached_at: Instant::now(),
                policy_generation: RESOURCE_FLIGHTS.policy_generation.load(Ordering::Acquire),
                proxy_generation: crate::backend::http::proxy_generation(),
                origin: CacheOrigin::Local,
            },
        );
        true
    }
}

impl ResourceFlightRegistry {
    /// Expose only aggregate in-flight work for the isolated acceptance
    /// matrix. The matrix needs to prove that its seeded local resource did
    /// not start a resolver request; no URLs or subscriber identities leave
    /// this registry.
    pub fn benchmark_pending_counts(&self) -> (usize, usize) {
        let flights = self.flights.lock_or_recover();
        (flights.active.len(), flights.deferred.len())
    }
}

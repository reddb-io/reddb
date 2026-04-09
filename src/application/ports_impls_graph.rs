use super::*;
impl RuntimeGraphPort for RedDBRuntime {
    fn resolve_graph_projection(
        &self,
        name: Option<&str>,
        inline: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<Option<RuntimeGraphProjection>> {
        RedDBRuntime::resolve_graph_projection(self, name, inline)
    }

    fn graph_neighborhood(
        &self,
        node: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphNeighborhoodResult> {
        RedDBRuntime::graph_neighborhood(self, node, direction, max_depth, edge_labels, projection)
    }

    fn graph_traverse(
        &self,
        source: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        strategy: RuntimeGraphTraversalStrategy,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTraversalResult> {
        RedDBRuntime::graph_traverse(
            self,
            source,
            direction,
            max_depth,
            strategy,
            edge_labels,
            projection,
        )
    }

    fn graph_shortest_path(
        &self,
        source: &str,
        target: &str,
        direction: RuntimeGraphDirection,
        algorithm: RuntimeGraphPathAlgorithm,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphPathResult> {
        RedDBRuntime::graph_shortest_path(
            self,
            source,
            target,
            direction,
            algorithm,
            edge_labels,
            projection,
        )
    }

    fn graph_components(
        &self,
        mode: RuntimeGraphComponentsMode,
        min_size: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphComponentsResult> {
        RedDBRuntime::graph_components(self, mode, min_size, projection)
    }

    fn graph_centrality(
        &self,
        algorithm: RuntimeGraphCentralityAlgorithm,
        top_k: usize,
        normalize: bool,
        max_iterations: Option<usize>,
        epsilon: Option<f64>,
        alpha: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        RedDBRuntime::graph_centrality(
            self,
            algorithm,
            top_k,
            normalize,
            max_iterations,
            epsilon,
            alpha,
            projection,
        )
    }

    fn graph_communities(
        &self,
        algorithm: crate::runtime::RuntimeGraphCommunityAlgorithm,
        min_size: usize,
        max_iterations: Option<usize>,
        resolution: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCommunityResult> {
        RedDBRuntime::graph_communities(
            self,
            algorithm,
            min_size,
            max_iterations,
            resolution,
            projection,
        )
    }

    fn graph_clustering(
        &self,
        top_k: usize,
        include_triangles: bool,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphClusteringResult> {
        RedDBRuntime::graph_clustering(self, top_k, include_triangles, projection)
    }

    fn graph_personalized_pagerank(
        &self,
        seeds: Vec<String>,
        top_k: usize,
        alpha: Option<f64>,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        RedDBRuntime::graph_personalized_pagerank(
            self,
            seeds,
            top_k,
            alpha,
            epsilon,
            max_iterations,
            projection,
        )
    }

    fn graph_hits(
        &self,
        top_k: usize,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphHitsResult> {
        RedDBRuntime::graph_hits(self, top_k, epsilon, max_iterations, projection)
    }

    fn graph_cycles(
        &self,
        max_length: usize,
        max_cycles: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCyclesResult> {
        RedDBRuntime::graph_cycles(self, max_length, max_cycles, projection)
    }

    fn graph_topological_sort(
        &self,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTopologicalSortResult> {
        RedDBRuntime::graph_topological_sort(self, projection)
    }
}

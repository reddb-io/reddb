use crate::application::ports::RuntimeGraphPort;
use crate::runtime::{
    RuntimeGraphCentralityAlgorithm, RuntimeGraphCentralityResult, RuntimeGraphClusteringResult,
    RuntimeGraphCommunityAlgorithm, RuntimeGraphCommunityResult, RuntimeGraphComponentsMode,
    RuntimeGraphComponentsResult, RuntimeGraphCyclesResult, RuntimeGraphDirection,
    RuntimeGraphHitsResult, RuntimeGraphNeighborhoodResult, RuntimeGraphPathAlgorithm,
    RuntimeGraphPathResult, RuntimeGraphProjection, RuntimeGraphTopologicalSortResult,
    RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy,
};
use crate::RedDBResult;

#[derive(Debug, Clone)]
pub struct GraphNeighborhoodInput {
    pub node: String,
    pub direction: RuntimeGraphDirection,
    pub max_depth: usize,
    pub edge_labels: Option<Vec<String>>,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphTraversalInput {
    pub source: String,
    pub direction: RuntimeGraphDirection,
    pub max_depth: usize,
    pub strategy: RuntimeGraphTraversalStrategy,
    pub edge_labels: Option<Vec<String>>,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphShortestPathInput {
    pub source: String,
    pub target: String,
    pub direction: RuntimeGraphDirection,
    pub algorithm: RuntimeGraphPathAlgorithm,
    pub edge_labels: Option<Vec<String>>,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphComponentsInput {
    pub mode: RuntimeGraphComponentsMode,
    pub min_size: usize,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphCentralityInput {
    pub algorithm: RuntimeGraphCentralityAlgorithm,
    pub top_k: usize,
    pub normalize: bool,
    pub max_iterations: Option<usize>,
    pub epsilon: Option<f64>,
    pub alpha: Option<f64>,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphCommunitiesInput {
    pub algorithm: RuntimeGraphCommunityAlgorithm,
    pub min_size: usize,
    pub max_iterations: Option<usize>,
    pub resolution: Option<f64>,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphClusteringInput {
    pub top_k: usize,
    pub include_triangles: bool,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphPersonalizedPageRankInput {
    pub seeds: Vec<String>,
    pub top_k: usize,
    pub alpha: Option<f64>,
    pub epsilon: Option<f64>,
    pub max_iterations: Option<usize>,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphHitsInput {
    pub top_k: usize,
    pub epsilon: Option<f64>,
    pub max_iterations: Option<usize>,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphCyclesInput {
    pub max_length: usize,
    pub max_cycles: usize,
    pub projection: Option<RuntimeGraphProjection>,
}

#[derive(Debug, Clone)]
pub struct GraphTopologicalSortInput {
    pub projection: Option<RuntimeGraphProjection>,
}

pub struct GraphUseCases<'a, P: ?Sized> {
    runtime: &'a P,
}

impl<'a, P: RuntimeGraphPort + ?Sized> GraphUseCases<'a, P> {
    pub fn new(runtime: &'a P) -> Self {
        Self { runtime }
    }

    pub fn neighborhood(
        &self,
        input: GraphNeighborhoodInput,
    ) -> RedDBResult<RuntimeGraphNeighborhoodResult> {
        self.runtime.graph_neighborhood(
            &input.node,
            input.direction,
            input.max_depth,
            input.edge_labels,
            input.projection,
        )
    }

    pub fn traverse(&self, input: GraphTraversalInput) -> RedDBResult<RuntimeGraphTraversalResult> {
        self.runtime.graph_traverse(
            &input.source,
            input.direction,
            input.max_depth,
            input.strategy,
            input.edge_labels,
            input.projection,
        )
    }

    pub fn shortest_path(
        &self,
        input: GraphShortestPathInput,
    ) -> RedDBResult<RuntimeGraphPathResult> {
        self.runtime.graph_shortest_path(
            &input.source,
            &input.target,
            input.direction,
            input.algorithm,
            input.edge_labels,
            input.projection,
        )
    }

    pub fn components(
        &self,
        input: GraphComponentsInput,
    ) -> RedDBResult<RuntimeGraphComponentsResult> {
        self.runtime
            .graph_components(input.mode, input.min_size, input.projection)
    }

    pub fn centrality(
        &self,
        input: GraphCentralityInput,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        self.runtime.graph_centrality(
            input.algorithm,
            input.top_k,
            input.normalize,
            input.max_iterations,
            input.epsilon,
            input.alpha,
            input.projection,
        )
    }

    pub fn communities(
        &self,
        input: GraphCommunitiesInput,
    ) -> RedDBResult<RuntimeGraphCommunityResult> {
        self.runtime.graph_communities(
            input.algorithm,
            input.min_size,
            input.max_iterations,
            input.resolution,
            input.projection,
        )
    }

    pub fn clustering(
        &self,
        input: GraphClusteringInput,
    ) -> RedDBResult<RuntimeGraphClusteringResult> {
        self.runtime
            .graph_clustering(input.top_k, input.include_triangles, input.projection)
    }

    pub fn personalized_pagerank(
        &self,
        input: GraphPersonalizedPageRankInput,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        self.runtime.graph_personalized_pagerank(
            input.seeds,
            input.top_k,
            input.alpha,
            input.epsilon,
            input.max_iterations,
            input.projection,
        )
    }

    pub fn hits(&self, input: GraphHitsInput) -> RedDBResult<RuntimeGraphHitsResult> {
        self.runtime.graph_hits(
            input.top_k,
            input.epsilon,
            input.max_iterations,
            input.projection,
        )
    }

    pub fn cycles(&self, input: GraphCyclesInput) -> RedDBResult<RuntimeGraphCyclesResult> {
        self.runtime
            .graph_cycles(input.max_length, input.max_cycles, input.projection)
    }

    pub fn topological_sort(
        &self,
        input: GraphTopologicalSortInput,
    ) -> RedDBResult<RuntimeGraphTopologicalSortResult> {
        self.runtime.graph_topological_sort(input.projection)
    }

    pub fn resolve_projection(
        &self,
        name: Option<&str>,
        inline: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<Option<RuntimeGraphProjection>> {
        self.runtime.resolve_graph_projection(name, inline)
    }
}

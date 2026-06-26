#![allow(dead_code)]

#[path = "support/mod.rs"]
mod support;

#[path = "grouped/surface_contracts/compile_fail.rs"]
mod compile_fail;

#[path = "grouped/surface_contracts/cross_binary_smoke.rs"]
mod cross_binary_smoke;

#[path = "grouped/surface_contracts/integration_rpc_stdio.rs"]
mod integration_rpc_stdio;

#[path = "grouped/surface_contracts/public_surface_contract_matrix.rs"]
mod public_surface_contract_matrix;

#[path = "grouped/surface_contracts/reddb_client_embedded.rs"]
mod reddb_client_embedded;

#[path = "grouped/surface_contracts/regress.rs"]
mod regress;

#[path = "grouped/surface_contracts/rql_conformance.rs"]
mod rql_conformance;

#[path = "grouped/surface_contracts/rql_reddb_conformance.rs"]
mod rql_reddb_conformance;

#[path = "grouped/surface_contracts/rql_sqlite_equivalence.rs"]
mod rql_sqlite_equivalence;

#[path = "grouped/surface_contracts/smoke_embedded.rs"]
mod smoke_embedded;

#[path = "grouped/surface_contracts/cross_transport_result_equivalence.rs"]
mod cross_transport_result_equivalence;

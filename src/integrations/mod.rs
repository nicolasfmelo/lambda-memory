pub mod database;
pub mod external_apis;
pub mod memory_ports;
#[cfg(any(test, feature = "test-support"))]
mod mock_terms;
pub mod provider_config;

pub mod capture_store;

pub mod consolidation_store;

pub mod embedding_ports;

pub mod openrouter_embeddings;

#[cfg(any(test, feature = "test-support"))]
pub mod mock_vector_repository;

pub mod derivation_ports;

#[cfg(feature = "memory-runtime")]
pub mod action_episodes;
#[cfg(feature = "memory-runtime")]
pub mod runtime_config;
#[cfg(feature = "memory-runtime")]
pub mod runtime_embeddings;

#[cfg(feature = "memory-runtime")]
pub mod exchange_content;

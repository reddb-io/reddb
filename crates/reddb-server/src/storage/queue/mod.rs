//! Queue / Deque Storage Module
//!
//! FIFO/LIFO message queue with:
//! - Push/Pop from both ends (deque)
//! - Consumer groups with acknowledgment
//! - Dead-letter queue support
//! - Priority queue mode
//! - Per-message TTL

pub mod consumer_group;
pub mod deque;

pub use consumer_group::{ConsumerGroup, PendingEntry};
pub use deque::{QueueSide, QueueStore};

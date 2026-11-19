//! The Frame rate limit choice, shared by the launcher's settings and the live-settings wire.
//!
//! The type, with the reasoning for its choices, moved to `vanta-protocol`
//! (`settings::FrameRateLimit`) beside the wire that carries it. This keeps the
//! path every caller already uses.

pub use vanta_protocol::FrameRateLimit;

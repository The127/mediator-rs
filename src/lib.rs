//! CQRS-style mediator with pipeline behaviors. Register handlers for command
//! types; dispatch routes to the right handler through a configurable behavior
//! pipeline.

mod extensions;
mod mediator;

pub use extensions::Extensions;
pub use mediator::{Handler, Mediator, MediatorError, PipelineBehavior, PipelineNext, Request};

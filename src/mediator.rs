use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use thiserror::Error;

use crate::Extensions;

/// Marker trait for commands. Associate an `Output` type with each command.
pub trait Request: Send + Sync + 'static {
    type Output: Send + Sync + 'static;

    /// Return extension metadata to be passed through the behavior pipeline.
    fn extensions(&self) -> Extensions {
        Extensions::new()
    }
}

/// Handles a specific command type, producing `CMD::Output` or `ERR`.
#[async_trait]
pub trait Handler<CMD, ERR, CTX>: Send + Sync
where
    CMD: Request,
{
    async fn handle(&self, cmd: CMD, ctx: &CTX) -> Result<CMD::Output, ERR>;
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type ErasedHandler<CTX, ERR> =
    dyn Fn(Box<dyn Any + Send + Sync>, &CTX) -> BoxFuture<Result<Box<dyn Any + Send + Sync>, ERR>>
        + Send
        + Sync;

type PipelineFn<'a, ERR> =
    Box<dyn FnOnce() -> BoxFuture<'a, Result<Box<dyn Any + Send + Sync>, ERR>> + Send + 'a>;

/// Continuation passed to each [`PipelineBehavior`]. Call `run` to invoke the
/// next behavior (or the terminal handler) in the chain.
pub struct PipelineNext<'a, ERR> {
    f: PipelineFn<'a, ERR>,
}

impl<'a, ERR> PipelineNext<'a, ERR> {
    pub async fn run(self) -> Result<Box<dyn Any + Send + Sync>, ERR> {
        (self.f)().await
    }
}

/// Middleware that wraps command dispatch. Receive the request's [`Extensions`]
/// and a `next` continuation; call `next.run()` to proceed down the pipeline.
#[async_trait]
pub trait PipelineBehavior<CTX, ERR>: Send + Sync {
    async fn handle(
        &self,
        extensions: &Extensions,
        ctx: &CTX,
        next: PipelineNext<'_, ERR>,
    ) -> Result<Box<dyn Any + Send + Sync>, ERR>;
}

/// Error returned from [`Mediator::dispatch`].
#[derive(Debug, Error)]
pub enum MediatorError<ERR: Debug> {
    #[error("No handler registered for {0:?}")]
    NoHandlerRegistered(TypeId),
    #[error("Handler error: {0:?}")]
    HandlerError(ERR),
}

/// Routes commands to registered handlers through an ordered behavior pipeline.
pub struct Mediator<CTX, ERR: Debug> {
    handlers: HashMap<TypeId, Box<ErasedHandler<CTX, ERR>>>,
    behaviors: Vec<std::sync::Arc<dyn PipelineBehavior<CTX, ERR>>>,
}

impl<CTX, ERR> Mediator<CTX, ERR>
where
    CTX: Send + Sync + 'static,
    ERR: Send + Debug + 'static,
{
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            behaviors: Vec::new(),
        }
    }

    /// Append a behavior to the pipeline (behaviors run in insertion order).
    pub fn add_behavior(&mut self, behavior: std::sync::Arc<dyn PipelineBehavior<CTX, ERR>>) {
        self.behaviors.push(behavior);
    }

    /// Register a handler for command type `CMD`.
    pub fn register<CMD, H>(&mut self, handler: H)
    where
        CMD: Request + 'static,
        CMD::Output: Send + 'static,
        H: Handler<CMD, ERR, CTX> + Send + Sync + 'static,
    {
        let handler = std::sync::Arc::new(handler);
        self.handlers.insert(
            TypeId::of::<CMD>(),
            Box::new(move |command, ctx| {
                let handler = handler.clone();
                Box::pin(async move {
                    let cmd = *command.downcast::<CMD>().expect("Invalid command type");
                    let result = handler.handle(cmd, ctx).await?;
                    Ok(Box::new(result) as Box<dyn Any + Send + Sync>)
                })
            }),
        );
    }

    /// Dispatch `command` through the behavior pipeline to its registered handler.
    pub async fn dispatch<CMD>(
        &self,
        command: CMD,
        ctx: &CTX,
    ) -> Result<CMD::Output, MediatorError<ERR>>
    where
        CMD: Request + 'static,
        CMD::Output: Send + 'static,
    {
        let handler = self
            .handlers
            .get(&TypeId::of::<CMD>())
            .ok_or(MediatorError::NoHandlerRegistered(TypeId::of::<CMD>()))?;

        let extensions = command.extensions();
        let result =
            Self::run_pipeline(&self.behaviors, &extensions, ctx, handler, Box::new(command))
                .await
                .map_err(MediatorError::HandlerError)?;

        Ok(*result
            .downcast::<CMD::Output>()
            .expect("Invalid handler output"))
    }

    fn run_pipeline<'a>(
        behaviors: &'a [std::sync::Arc<dyn PipelineBehavior<CTX, ERR>>],
        extensions: &'a Extensions,
        ctx: &'a CTX,
        handler: &'a ErasedHandler<CTX, ERR>,
        command: Box<dyn Any + Send + Sync>,
    ) -> BoxFuture<'a, Result<Box<dyn Any + Send + Sync>, ERR>> {
        Box::pin(async move {
            if behaviors.is_empty() {
                return handler(command, ctx).await;
            }

            let (first, rest) = behaviors.split_first().unwrap();
            let next = PipelineNext {
                f: Box::new(move || Self::run_pipeline(rest, extensions, ctx, handler, command)),
            };

            first.handle(extensions, ctx, next).await
        })
    }
}

impl<CTX, ERR> Default for Mediator<CTX, ERR>
where
    CTX: Send + Sync + 'static,
    ERR: Send + Debug + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockCommand {
        id: u32,
    }

    impl Request for MockCommand {
        type Output = String;
    }

    struct MockHandler;

    #[async_trait]
    impl Handler<MockCommand, String, String> for MockHandler {
        async fn handle(&self, cmd: MockCommand, ctx: &String) -> Result<String, String> {
            Ok(format!("Handled {} with context {}", cmd.id, ctx))
        }
    }

    #[tokio::test]
    async fn given_a_registered_handler_when_dispatching_a_command_it_should_return_the_expected_result()
    {
        // arrange
        let mut mediator = Mediator::new();
        mediator.register::<MockCommand, _>(MockHandler);
        let cmd = MockCommand { id: 1 };
        let ctx = "test-context".to_string();

        // act
        let result = mediator.dispatch(cmd, &ctx).await;

        // assert
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Handled 1 with context test-context");
    }

    #[tokio::test]
    async fn given_no_registered_handler_when_dispatching_a_command_it_should_return_a_no_handler_error()
    {
        // arrange
        let mediator: Mediator<String, String> = Mediator::new();
        let cmd = MockCommand { id: 1 };
        let ctx = "test-context".to_string();

        // act
        let result = mediator.dispatch(cmd, &ctx).await;

        // assert
        match result {
            Err(MediatorError::NoHandlerRegistered(_)) => (),
            _ => panic!("Expected NoHandlerRegistered error"),
        }
    }

    struct FailingCommand;
    impl Request for FailingCommand {
        type Output = ();
    }

    struct FailingHandler;
    #[async_trait]
    impl Handler<FailingCommand, String, String> for FailingHandler {
        async fn handle(&self, _cmd: FailingCommand, _ctx: &String) -> Result<(), String> {
            Err("error message".to_string())
        }
    }

    #[tokio::test]
    async fn given_a_failing_handler_when_dispatching_a_command_it_should_return_the_handler_error()
    {
        // arrange
        let mut mediator = Mediator::new();
        mediator.register::<FailingCommand, _>(FailingHandler);
        let cmd = FailingCommand;
        let ctx = "test-context".to_string();

        // act
        let result = mediator.dispatch(cmd, &ctx).await;

        // assert
        match result {
            Err(MediatorError::HandlerError(err)) => assert_eq!(err, "error message"),
            _ => panic!("Expected HandlerError error, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn given_multiple_handlers_when_dispatching_different_commands_it_should_route_to_correct_handlers()
    {
        // arrange
        let mut mediator = Mediator::new();
        mediator.register::<MockCommand, _>(MockHandler);

        struct AnotherCommand;
        impl Request for AnotherCommand {
            type Output = u32;
        }
        struct AnotherHandler;
        #[async_trait]
        impl Handler<AnotherCommand, String, String> for AnotherHandler {
            async fn handle(&self, _cmd: AnotherCommand, _ctx: &String) -> Result<u32, String> {
                Ok(42)
            }
        }
        mediator.register::<AnotherCommand, _>(AnotherHandler);

        let ctx = "ctx".to_string();

        // act
        let res1 = mediator.dispatch(MockCommand { id: 1 }, &ctx).await;
        let res2 = mediator.dispatch(AnotherCommand, &ctx).await;

        // assert
        assert_eq!(res1.unwrap(), "Handled 1 with context ctx");
        assert_eq!(res2.unwrap(), 42);
    }
}

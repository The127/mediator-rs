# mediator-rs

A CQRS-style mediator for Rust: type-safe command dispatch through a configurable async pipeline.

## Motivation

In layered or vertical-slice architectures it's tempting to have application services call each
other directly. This creates hidden coupling — a service that sends email, audits the action, and
validates a quota all at once is hard to test in isolation and impossible to reuse individually.

A mediator breaks that coupling. Each operation is expressed as a **command** (a plain Rust struct)
and routed to exactly one **handler**. Cross-cutting concerns — logging, tracing, authorization
checks, retry logic — live in **pipeline behaviors** that wrap every dispatch without touching
handler code. Callers know only the command type; the mediator knows only how to route it.

## Installation

```toml
[dependencies]
mediator-rs = { path = "..." }  # or publish to crates.io and use version = "..."
async-trait = "0.1"
```

## Usage

### Define a command

Implement `Request` to associate a command struct with its output type.

```rust
use mediator_rs::Request;

pub struct CreateUser {
    pub username: String,
    pub email: String,
}

impl Request for CreateUser {
    type Output = u64; // newly created user ID
}
```

### Write a handler

```rust
use async_trait::async_trait;
use mediator_rs::Handler;

pub struct AppContext {
    // database pool, config, etc.
}

pub struct CreateUserHandler;

#[async_trait]
impl Handler<CreateUser, String, AppContext> for CreateUserHandler {
    async fn handle(&self, cmd: CreateUser, ctx: &AppContext) -> Result<u64, String> {
        // insert into database via ctx ...
        Ok(42)
    }
}
```

The three generic parameters are `<CMD, ERR, CTX>`: the command type, the error type shared across
this mediator instance, and the application context passed to every handler.

### Write a pipeline behavior

Behaviors implement `PipelineBehavior<CTX, ERR>`. They receive the command's `Extensions` map, the
shared context, and a `PipelineNext` continuation. Call `next.run()` to proceed; skip it to
short-circuit (e.g. an authorization check that fails early).

```rust
use async_trait::async_trait;
use std::any::Any;
use mediator_rs::{Extensions, PipelineBehavior, PipelineNext};

pub struct LoggingBehavior;

#[async_trait]
impl PipelineBehavior<AppContext, String> for LoggingBehavior {
    async fn handle(
        &self,
        extensions: &Extensions,
        ctx: &AppContext,
        next: PipelineNext<'_, String>,
    ) -> Result<Box<dyn Any + Send + Sync>, String> {
        println!("[before] dispatching command");
        let result = next.run().await;
        println!("[after]  dispatch complete, ok={}", result.is_ok());
        result
    }
}
```

### Register and dispatch

```rust
use std::sync::Arc;
use mediator_rs::Mediator;

let mut mediator: Mediator<AppContext, String> = Mediator::new();

// Behaviors run in insertion order, outermost first.
mediator.add_behavior(Arc::new(LoggingBehavior));

// One handler per command type.
mediator.register::<CreateUser, _>(CreateUserHandler);

let ctx = AppContext { /* ... */ };

let user_id = mediator
    .dispatch(CreateUser { username: "alice".into(), email: "alice@example.com".into() }, &ctx)
    .await
    .expect("dispatch failed");
```

`dispatch` returns `Result<CMD::Output, MediatorError<ERR>>`. `MediatorError` is either
`NoHandlerRegistered` (a programming error caught at runtime) or `HandlerError(ERR)` (a domain
error propagated from the handler).

### Attaching metadata via Extensions

`Extensions` is a `TypeId`-keyed map. Commands can expose metadata to behaviors without adding
behavior-specific fields to the command struct. Override `Request::extensions()`:

```rust
use mediator_rs::{Extensions, Request};

pub struct RequestId(pub String);

pub struct DeleteProject {
    pub project_id: u64,
    pub request_id: String,
}

impl Request for DeleteProject {
    type Output = ();

    fn extensions(&self) -> Extensions {
        let mut ext = Extensions::new();
        ext.insert(RequestId(self.request_id.clone()));
        ext
    }
}
```

Read the value in any behavior:

```rust
if let Some(rid) = extensions.get::<RequestId>() {
    println!("request_id={}", rid.0);
}
```

A tracing behavior doesn't need to know about `DeleteProject`; it only needs `RequestId`. This is
how you keep behaviors decoupled from command types.

## Design notes

### TypeId-based dispatch

`Mediator` stores handlers in a `HashMap<TypeId, Box<ErasedHandler>>`. When a command is
registered, its concrete handler is wrapped in a closure that downcasts the erased `Box<dyn Any>`
back to the original type. Dispatch is a single hash lookup with no reflection overhead beyond the
downcast.

### PipelineNext and behavior chaining

`PipelineNext` owns a `Box<dyn FnOnce() -> BoxFuture<...>>`. Calling `run()` recursively builds
the next `PipelineNext` from the remaining behavior slice, bottoming out at the terminal handler.
The first registered behavior is the outermost wrapper:

```
dispatch()
  └─ Behavior[0].handle(extensions, ctx, next)
       └─ Behavior[1].handle(extensions, ctx, next)
            └─ ...
                 └─ Handler.handle(cmd, ctx)
```

### Extensions lifetime

`Extensions` is constructed once from `cmd.extensions()` before the pipeline runs, then borrowed
by every behavior. The original command is moved into the erased handler closure. Behaviors never
hold a reference to the command — only to the metadata the command chose to expose.

## License

MIT

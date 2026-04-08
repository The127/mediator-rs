# mediator-rs

A CQRS-style mediator for Rust: type-safe command dispatch through a configurable async pipeline.

## Motivation

In layered or vertical-slice architectures it is tempting to have application services call each
other directly. This creates hidden coupling — a service that sends email, audits the action, and
validates a quota all at once is hard to test in isolation and impossible to reuse individually.

A mediator breaks that coupling. Each operation is expressed as a **command** (a plain Rust struct)
and routed to exactly one **handler**. Cross-cutting concerns — logging, tracing, authorization
checks, retry logic — live in **pipeline behaviors** that wrap every dispatch without touching
handler code. The result is a clean CQRS-style boundary: callers know only the command type; the
mediator knows only how to route it.

## Features

- **Type-safe dispatch** — commands are matched to handlers at registration time via `TypeId`; no
  string keys or `dyn Any` leaking into application code
- **Async-native** — handlers and behaviors are `async fn`; no blocking, no executor assumptions
- **Pipeline behaviors** — ordered middleware that wraps every dispatch; compose logging, auth,
  validation, and metrics independently
- **Extensions metadata map** — attach arbitrary typed values to a command and read them in any
  behavior without adding fields to the command struct
- **Zero handler boilerplate** — one trait impl per command; the mediator owns routing and
  lifecycle

## Installation

```toml
[dependencies]
mediator-rs = { path = "..." }  # or publish to crates.io and use version = "..."
async-trait = "0.1"
```

## Usage

### 1. Define a command

Implement `Request` to declare the command and its output type.

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

### 2. Write a handler

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
        println!("Creating user: {}", cmd.username);
        Ok(42)
    }
}
```

The three generic parameters are `<CMD, ERR, CTX>`:

| Parameter | Meaning                          |
|-----------|----------------------------------|
| `CMD`     | The command type being handled   |
| `ERR`     | The error type for this mediator |
| `CTX`     | Shared application context       |

### 3. Write a pipeline behavior

Behaviors implement `PipelineBehavior<CTX, ERR>`. They receive the command's `Extensions` map, the
shared context, and a `PipelineNext` continuation. Calling `next.run()` proceeds to the next
behavior or the terminal handler.

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

Behaviors that need to short-circuit (e.g. an authorization check) simply return an `Err` without
calling `next.run()`.

### 4. Wire up the mediator

```rust
use std::sync::Arc;
use mediator_rs::Mediator;

let mut mediator: Mediator<AppContext, String> = Mediator::new();

// Behaviors run in insertion order, outermost first.
mediator.add_behavior(Arc::new(LoggingBehavior));

// Register one handler per command type.
mediator.register::<CreateUser, _>(CreateUserHandler);
```

### 5. Dispatch a command

```rust
let ctx = AppContext { /* ... */ };

let user_id = mediator
    .dispatch(CreateUser { username: "alice".into(), email: "alice@example.com".into() }, &ctx)
    .await
    .expect("dispatch failed");

println!("Created user with id {user_id}");
```

`dispatch` returns `Result<CMD::Output, MediatorError<ERR>>`. `MediatorError` is either
`NoHandlerRegistered` (a programming error caught at runtime) or `HandlerError(ERR)` (a domain
error from the handler).

### 6. Attaching metadata via Extensions

`Extensions` is a `TypeId`-keyed map. Attach values by overriding `Request::extensions()`; read
them in any behavior.

```rust
use mediator_rs::{Extensions, Request};

// A metadata value — any Send + Sync + 'static type works.
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

Read the value inside a behavior:

```rust
#[async_trait]
impl PipelineBehavior<AppContext, String> for LoggingBehavior {
    async fn handle(
        &self,
        extensions: &Extensions,
        ctx: &AppContext,
        next: PipelineNext<'_, String>,
    ) -> Result<Box<dyn Any + Send + Sync>, String> {
        if let Some(rid) = extensions.get::<RequestId>() {
            println!("request_id={}", rid.0);
        }
        next.run().await
    }
}
```

This keeps behaviors generic: a tracing behavior does not need to know about `DeleteProject`; it
only needs to know about `RequestId`.

## Design notes

### TypeId-based dispatch

`Mediator` stores handlers in a `HashMap<TypeId, Box<ErasedHandler>>`. When a command is
registered, its concrete handler is wrapped in a closure that downcasts the erased `Box<dyn Any>`
back to the original type. Because each command type has a unique `TypeId`, dispatch is a single
hash lookup with no reflection overhead beyond the downcast.

### PipelineNext and behavior chaining

`PipelineNext` owns a `Box<dyn FnOnce() -> BoxFuture<...>>`. When `run()` is called, that closure
recursively builds the next `PipelineNext` from the remaining slice of behaviors — eventually
bottoming out at the terminal handler. Behaviors are therefore applied as a stack: the first
registered behavior is the outermost wrapper and runs both before and after all others.

```
dispatch()
  └─ Behavior[0].handle(extensions, ctx, next)
       └─ Behavior[1].handle(extensions, ctx, next)
            └─ ...
                 └─ Handler.handle(cmd, ctx)
```

Because each layer simply calls `next.run().await` and awaits the result, adding a behavior has no
effect on handler code and behaviors are fully composable.

### Extensions lifetime

`Extensions` is constructed once from `cmd.extensions()` before the pipeline runs, then borrowed
by every behavior. The original command is moved into the erased handler box. Behaviors therefore
never hold a reference to the command itself — only to the metadata the command chose to expose.

## License

MIT

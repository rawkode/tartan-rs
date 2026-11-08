use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimitsBuilder};

use crate::bindings::{Extension, ExtensionPre, types};
use crate::host_state::{CallKind, Effect, HostState};
use crate::{ExtensionError, SqlBackend};

/// Compiles extension components and runs their exports.
#[derive(Clone)]
pub struct ExtensionRuntime {
    engine: Engine,
    linker: Arc<Linker<HostState>>,
}

/// A compiled component, ready to instantiate. Cloning is cheap.
#[derive(Clone)]
pub struct ExtensionComponent {
    pre: ExtensionPre<HostState>,
    digest: String,
}

/// The resources one call may use.
#[derive(Clone, Copy, Debug)]
pub struct CallLimits {
    /// Fuel, roughly one unit per WebAssembly instruction.
    pub fuel: u64,
    /// The most linear memory the instance may grow to, in bytes.
    pub memory_bytes: usize,
    /// Wall-clock time, host calls included.
    pub timeout: Duration,
}

impl Default for CallLimits {
    fn default() -> Self {
        Self {
            fuel: 2_000_000_000,
            memory_bytes: 128 << 20,
            timeout: Duration::from_secs(10),
        }
    }
}

/// What a call returned, and the effects to apply because it succeeded. A call that returns an
/// extension error has no effects.
#[derive(Debug)]
pub struct CallOutcome<T> {
    /// The export's result.
    pub result: Result<T, ExtensionError>,
    /// The effects the call buffered; empty unless `result` is `Ok`.
    pub effects: Vec<Effect>,
}

/// The environment of one call.
pub struct CallEnvironment {
    /// The installation's database.
    pub sql: Arc<dyn SqlBackend>,
    /// The installation's configuration, as JSON.
    pub config: String,
    /// The call's limits.
    pub limits: CallLimits,
    /// Names the installation in logs.
    pub label: String,
}

impl ExtensionRuntime {
    /// A runtime with fuel metering and asynchronous host calls.
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        Extension::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |state| state)?;
        Ok(Self {
            engine,
            linker: Arc::new(linker),
        })
    }

    /// Compiles a component and checks it fits the `tartan:ext` world.
    pub fn compile(&self, bytes: &[u8]) -> Result<ExtensionComponent> {
        let component = Component::new(&self.engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile the component")?;
        let instance_pre = self
            .linker
            .instantiate_pre(&component)
            .map_err(anyhow::Error::from)
            .context("the component does not fit the tartan:ext world")?;
        Ok(ExtensionComponent {
            pre: ExtensionPre::new(instance_pre)?,
            digest: hex::encode(Sha256::digest(bytes)),
        })
    }
}

impl ExtensionComponent {
    /// The SHA-256 of the component's bytes.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Calls `init`.
    pub async fn init(&self, env: CallEnvironment) -> Result<CallOutcome<()>> {
        let timeout = env.limits.timeout;
        let (mut store, extension) = self.instantiate(env, CallKind::ReadWrite).await?;
        let result = with_timeout(timeout, extension.call_init(&mut store)).await?;
        Ok(finish(store, result))
    }

    /// Calls `call-tool`.
    pub async fn call_tool(
        &self,
        env: CallEnvironment,
        name: &str,
        args: &str,
        context: &types::SlotContext,
    ) -> Result<CallOutcome<String>> {
        let timeout = env.limits.timeout;
        let (mut store, extension) = self.instantiate(env, CallKind::ReadWrite).await?;
        let (name, args) = (name.to_owned(), args.to_owned());
        let call = extension.call_call_tool(&mut store, &name, &args, context);
        let result = with_timeout(timeout, call).await?;
        Ok(finish(store, result))
    }

    /// Calls `render`, which may only read.
    pub async fn render(
        &self,
        env: CallEnvironment,
        slot: &str,
        context: &types::SlotContext,
        props: &str,
    ) -> Result<CallOutcome<String>> {
        let timeout = env.limits.timeout;
        let (mut store, extension) = self.instantiate(env, CallKind::ReadOnly).await?;
        let (slot, props) = (slot.to_owned(), props.to_owned());
        let call = extension.call_render(&mut store, &slot, context, &props);
        let result = with_timeout(timeout, call).await?;
        Ok(finish(store, result))
    }

    /// Calls `on-action`.
    pub async fn on_action(
        &self,
        env: CallEnvironment,
        action: &str,
        payload: &str,
        context: &types::SlotContext,
    ) -> Result<CallOutcome<String>> {
        let timeout = env.limits.timeout;
        let (mut store, extension) = self.instantiate(env, CallKind::ReadWrite).await?;
        let (action, payload) = (action.to_owned(), payload.to_owned());
        let call = extension.call_on_action(&mut store, &action, &payload, context);
        let result = with_timeout(timeout, call).await?;
        Ok(finish(store, result))
    }

    /// Calls `on-event`.
    pub async fn on_event(
        &self,
        env: CallEnvironment,
        event: &types::Event,
    ) -> Result<CallOutcome<()>> {
        let timeout = env.limits.timeout;
        let (mut store, extension) = self.instantiate(env, CallKind::ReadWrite).await?;
        let result = with_timeout(timeout, extension.call_on_event(&mut store, event)).await?;
        Ok(finish(store, result))
    }

    /// Calls `on-timer`.
    pub async fn on_timer(&self, env: CallEnvironment, key: &str) -> Result<CallOutcome<()>> {
        let timeout = env.limits.timeout;
        let (mut store, extension) = self.instantiate(env, CallKind::ReadWrite).await?;
        let key = key.to_owned();
        let result = with_timeout(timeout, extension.call_on_timer(&mut store, &key)).await?;
        Ok(finish(store, result))
    }

    async fn instantiate(
        &self,
        env: CallEnvironment,
        kind: CallKind,
    ) -> Result<(Store<HostState>, Extension)> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(env.limits.memory_bytes)
            .instances(16)
            .tables(16)
            .memories(4)
            .build();
        let state = HostState::new(env.sql, env.config, kind, limits, env.label);
        let mut store = Store::new(self.pre.engine(), state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(env.limits.fuel)?;
        // Yield to the executor regularly, so the call's timeout can fire.
        store.fuel_async_yield_interval(Some(100_000))?;
        let extension = self.pre.instantiate_async(&mut store).await?;
        Ok((store, extension))
    }
}

async fn with_timeout<T>(
    timeout: Duration,
    call: impl Future<Output = wasmtime::Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(timeout, call).await {
        Ok(result) => Ok(result?),
        Err(_) => anyhow::bail!("the extension call took longer than {timeout:?}"),
    }
}

fn finish<T>(store: Store<HostState>, result: Result<T, ExtensionError>) -> CallOutcome<T> {
    let effects = if result.is_ok() {
        store.into_data().effects
    } else {
        Vec::new()
    };
    CallOutcome { result, effects }
}

#[cfg(not(feature = "js_runtime"))]
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PureJsTransformOptions {
    pub memory_limit_bytes: usize,
    pub stack_limit_bytes: usize,
    pub timeout: Duration,
}

impl Default for PureJsTransformOptions {
    fn default() -> Self {
        Self {
            memory_limit_bytes: 64 * 1024 * 1024,
            stack_limit_bytes: 256 * 1024,
            timeout: Duration::from_millis(100),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PureJsTransformError {
    Disabled,
    Message(String),
}

impl std::fmt::Display for PureJsTransformError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => {
                formatter.write_str("JavaScript transforms require Vulcan's `js_runtime` feature")
            }
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PureJsTransformError {}

#[cfg(not(feature = "js_runtime"))]
#[derive(Debug)]
pub struct PureJsTransform;

#[cfg(not(feature = "js_runtime"))]
impl PureJsTransform {
    pub fn new(
        _source: &str,
        _handler_name: &str,
        _options: PureJsTransformOptions,
    ) -> Result<Self, PureJsTransformError> {
        Err(PureJsTransformError::Disabled)
    }

    pub fn call(&self, _input: &Value) -> Result<Value, PureJsTransformError> {
        Err(PureJsTransformError::Disabled)
    }
}

#[cfg(feature = "js_runtime")]
mod runtime {
    use super::{PureJsTransformError, PureJsTransformOptions};
    use rquickjs::{CatchResultExt, CaughtError, Context, Runtime};
    use serde_json::Value;
    use std::time::Instant;

    const DETERMINISTIC_PRELUDE: &str = r#"
const __vulcanOriginalDate = Date;
function __vulcanNondeterministic(operation) {
  throw new Error(`export transforms do not allow nondeterministic ${operation}`);
}
globalThis.Date = new Proxy(__vulcanOriginalDate, {
  apply(target, thisArg, args) {
    if ((args?.length ?? 0) === 0) __vulcanNondeterministic("Date()");
    return Reflect.apply(target, thisArg, args);
  },
  construct(target, args, newTarget) {
    if ((args?.length ?? 0) === 0) __vulcanNondeterministic("new Date()");
    return Reflect.construct(target, args, newTarget);
  },
  get(target, prop, receiver) {
    if (prop === "now") return () => __vulcanNondeterministic("Date.now()");
    const value = Reflect.get(target, prop, receiver);
    return typeof value === "function" ? value.bind(target) : value;
  }
});
Math.random = () => __vulcanNondeterministic("Math.random()");
"#;

    pub struct PureJsTransform {
        runtime: Runtime,
        context: Context,
        handler_name: String,
        timeout: std::time::Duration,
    }

    impl std::fmt::Debug for PureJsTransform {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("PureJsTransform")
                .field("handler_name", &self.handler_name)
                .field("timeout", &self.timeout)
                .finish_non_exhaustive()
        }
    }

    impl PureJsTransform {
        pub fn new(
            source: &str,
            handler_name: &str,
            options: PureJsTransformOptions,
        ) -> Result<Self, PureJsTransformError> {
            if handler_name.trim().is_empty() {
                return Err(PureJsTransformError::Message(
                    "JavaScript transform handler name cannot be empty".to_string(),
                ));
            }
            if options.timeout.is_zero() {
                return Err(PureJsTransformError::Message(
                    "JavaScript transform timeout must be greater than zero".to_string(),
                ));
            }
            let runtime =
                Runtime::new().map_err(|error| PureJsTransformError::Message(error.to_string()))?;
            runtime.set_memory_limit(options.memory_limit_bytes);
            runtime.set_max_stack_size(options.stack_limit_bytes);
            let context = Context::full(&runtime)
                .map_err(|error| PureJsTransformError::Message(error.to_string()))?;
            let deadline = Instant::now();
            let timeout = options.timeout;
            runtime.set_interrupt_handler(Some(Box::new(move || deadline.elapsed() >= timeout)));
            let setup = context.with(|ctx| -> Result<(), PureJsTransformError> {
                ctx.eval::<(), _>(DETERMINISTIC_PRELUDE)
                    .catch(&ctx)
                    .map_err(|error| map_error(&error, timeout))?;
                ctx.eval::<(), _>(source)
                    .catch(&ctx)
                    .map_err(|error| map_error(&error, timeout))?;
                let serialized_handler =
                    serde_json::to_string(handler_name).map_err(message_error)?;
                let check = format!("typeof globalThis[{serialized_handler}] === 'function'");
                let exists = ctx
                    .eval::<bool, _>(check)
                    .catch(&ctx)
                    .map_err(|error| map_error(&error, timeout))?;
                if !exists {
                    return Err(PureJsTransformError::Message(format!(
                        "JavaScript transform handler `{handler_name}` is not defined"
                    )));
                }
                Ok(())
            });
            runtime.set_interrupt_handler(None);
            setup?;
            Ok(Self {
                runtime,
                context,
                handler_name: handler_name.to_string(),
                timeout,
            })
        }

        pub fn call(&self, input: &Value) -> Result<Value, PureJsTransformError> {
            let input_json = serde_json::to_string(input).map_err(message_error)?;
            let serialized_input = serde_json::to_string(&input_json).map_err(message_error)?;
            let serialized_handler =
                serde_json::to_string(&self.handler_name).map_err(message_error)?;
            let invocation = format!(
                "(() => {{\n\
const __vulcanTransformInput = JSON.parse({serialized_input});\n\
const __vulcanTransformHandler = globalThis[{serialized_handler}];\n\
const __vulcanTransformResult = __vulcanTransformHandler(__vulcanTransformInput);\n\
if (__vulcanTransformResult && typeof __vulcanTransformResult.then === 'function') {{\n\
  throw new Error('JavaScript transform handlers must return synchronously');\n\
}}\n\
if (__vulcanTransformResult === undefined) {{\n\
  throw new Error('JavaScript transform handler returned undefined');\n\
}}\n\
return JSON.stringify(__vulcanTransformResult);\n\
}})();"
            );
            let deadline = Instant::now();
            let timeout = self.timeout;
            self.runtime
                .set_interrupt_handler(Some(Box::new(move || deadline.elapsed() >= timeout)));
            let result = self.context.with(|ctx| {
                ctx.eval::<String, _>(invocation)
                    .catch(&ctx)
                    .map_err(|error| map_error(&error, timeout))
            });
            self.runtime.set_interrupt_handler(None);
            let serialized = result?;
            serde_json::from_str(&serialized).map_err(message_error)
        }
    }

    fn map_error(error: &CaughtError<'_>, timeout: std::time::Duration) -> PureJsTransformError {
        let message = error.to_string();
        if message.to_ascii_lowercase().contains("interrupted") {
            PureJsTransformError::Message(format!(
                "JavaScript transform timed out after {} ms",
                timeout.as_millis().max(1)
            ))
        } else {
            PureJsTransformError::Message(message.trim().to_string())
        }
    }

    fn message_error(error: impl std::fmt::Display) -> PureJsTransformError {
        PureJsTransformError::Message(error.to_string())
    }
}

#[cfg(feature = "js_runtime")]
pub use runtime::PureJsTransform;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "js_runtime")]
    use serde_json::{json, Value};

    #[cfg(feature = "js_runtime")]
    #[test]
    fn pure_transform_calls_a_typed_synchronous_handler() {
        let transform = PureJsTransform::new(
            "function transform(value) { return { replacement: value.label.toUpperCase() }; }",
            "transform",
            PureJsTransformOptions::default(),
        )
        .expect("transform should compile");
        assert_eq!(
            transform.call(&json!({"label": "alpha"})).unwrap(),
            json!({"replacement": "ALPHA"})
        );
    }

    #[cfg(not(feature = "js_runtime"))]
    #[test]
    fn pure_transform_reports_disabled_without_the_runtime_feature() {
        assert_eq!(
            PureJsTransform::new(
                "function transform(value) { return value; }",
                "transform",
                PureJsTransformOptions::default(),
            )
            .unwrap_err(),
            PureJsTransformError::Disabled
        );
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn pure_transform_rejects_nondeterminism_and_times_out() {
        let random = PureJsTransform::new(
            "function transform() { return Math.random(); }",
            "transform",
            PureJsTransformOptions::default(),
        )
        .expect("transform should compile");
        assert!(random
            .call(&Value::Null)
            .unwrap_err()
            .to_string()
            .contains("nondeterministic Math.random()"));

        let looping = PureJsTransform::new(
            "function transform() { while (true) {} }",
            "transform",
            PureJsTransformOptions {
                timeout: Duration::from_millis(10),
                ..PureJsTransformOptions::default()
            },
        )
        .expect("transform should compile");
        assert!(looping
            .call(&Value::Null)
            .unwrap_err()
            .to_string()
            .contains("timed out"));
    }
}

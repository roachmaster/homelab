use crate::{builtins, expression::method_name, schema, Limits, TemplateError, Value};
use std::{
    collections::{btree_map::Entry, BTreeMap},
    sync::Arc,
};

type Callback = dyn Fn(&Value, &[Value]) -> Result<Value, TemplateError> + Send + Sync + 'static;
enum Method {
    Builtin,
    Custom {
        callback: Arc<Callback>,
        arity: Option<usize>,
    },
}

/// Explicit method membership, local to the caller. Defaults contain the 38
/// documented builtins; empty registries contain none. Duplicates never replace.
/// Custom callbacks are trusted synchronous Rust code: limits cannot preempt,
/// undo effects, prevent callback allocations, or catch panics. Prefer pure
/// deterministic callbacks and keep capability policy outside templates.
pub struct MethodRegistry {
    methods: BTreeMap<String, Method>,
}
impl Default for MethodRegistry {
    fn default() -> Self {
        Self::new()
    }
}
impl MethodRegistry {
    pub fn empty() -> Self {
        Self {
            methods: BTreeMap::new(),
        }
    }
    pub fn new() -> Self {
        Self {
            methods: builtins::NAMES
                .iter()
                .map(|name| ((*name).to_owned(), Method::Builtin))
                .collect(),
        }
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.methods.keys().map(String::as_str)
    }
    pub fn register<F>(&mut self, name: impl Into<String>, method: F) -> Result<(), TemplateError>
    where
        F: Fn(&Value) -> Result<Value, TemplateError> + Send + Sync + 'static,
    {
        self.insert(name.into(), Arc::new(move |v, _| method(v)), Some(0))
    }
    pub fn register_with_args<F>(
        &mut self,
        name: impl Into<String>,
        method: F,
    ) -> Result<(), TemplateError>
    where
        F: Fn(&Value, &[Value]) -> Result<Value, TemplateError> + Send + Sync + 'static,
    {
        self.insert(name.into(), Arc::new(method), None)
    }
    fn insert(
        &mut self,
        name: String,
        callback: Arc<Callback>,
        arity: Option<usize>,
    ) -> Result<(), TemplateError> {
        if !method_name(&name) {
            return Err(error(&name, "invalid method name"));
        }
        match self.methods.entry(name.clone()) {
            Entry::Occupied(_) => Err(error(&name, "already registered; original preserved")),
            Entry::Vacant(entry) => {
                entry.insert(Method::Custom { callback, arity });
                Ok(())
            }
        }
    }
    pub(crate) fn check(&self, name: &str, count: usize) -> Result<bool, TemplateError> {
        match self.methods.get(name) {
            None => Err(error(name, "unknown method")),
            Some(Method::Builtin) => {
                let expected = match name {
                    "replace" => 2,
                    "default" | "contains" | "starts_with" | "ends_with" | "join" | "at"
                    | "get" | "has" | "add" | "subtract" | "multiply" | "divide" => 1,
                    _ => 0,
                };
                if count != expected {
                    return Err(error(name, "wrong argument count"));
                }
                Ok(true)
            }
            Some(Method::Custom { arity, .. }) => {
                if arity.is_some_and(|n| count != n) {
                    return Err(error(name, "wrong argument count"));
                }
                Ok(false)
            }
        }
    }
    pub(crate) fn call(
        &self,
        name: &str,
        value: &Value,
        args: &[Value],
        budget: &mut MethodBudget,
    ) -> Result<Value, TemplateError> {
        budget.tick()?;
        self.check(name, args.len())?;
        let result = match &self.methods[name] {
            Method::Builtin => builtins::call(name, value, args, budget)?,
            Method::Custom { callback, .. } => callback(value, args)?,
        };
        // This validates AFTER a custom callback returns, not during its execution.
        budget.validate(&result)?;
        Ok(result)
    }
}
pub(crate) fn error(name: &str, reason: &str) -> TemplateError {
    TemplateError::Method {
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

pub(crate) struct MethodBudget {
    pub(crate) limits: Limits,
    pub(crate) remaining: usize,
}
impl MethodBudget {
    pub(crate) fn tick(&mut self) -> Result<(), TemplateError> {
        self.charge(1)
    }
    pub(crate) fn charge(&mut self, count: usize) -> Result<(), TemplateError> {
        self.remaining = self
            .remaining
            .checked_sub(count)
            .ok_or(TemplateError::Limit("work steps"))?;
        Ok(())
    }
    pub(crate) fn push(&mut self, target: &mut String, text: &str) -> Result<(), TemplateError> {
        if target.len() > self.limits.max_data_bytes
            || text.len() > self.limits.max_data_bytes - target.len()
        {
            return Err(TemplateError::Limit("method value bytes"));
        }
        self.charge(text.len())?;
        target.push_str(text);
        Ok(())
    }
    pub(crate) fn validate(&mut self, value: &Value) -> Result<(), TemplateError> {
        schema::validate_value(value, self.limits)?;
        self.measure(value)
    }
    fn measure(&mut self, value: &Value) -> Result<(), TemplateError> {
        self.tick()?;
        match value {
            Value::Text(text) => self.charge(text.len())?,
            Value::List(items) => {
                for item in items {
                    self.measure(item)?;
                }
            }
            Value::Object(items) => {
                for (key, item) in items {
                    self.charge(key.len())?;
                    self.measure(item)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub(crate) fn copy(&mut self, value: &Value) -> Result<Value, TemplateError> {
        self.validate(value)?;
        Ok(value.clone())
    }
}

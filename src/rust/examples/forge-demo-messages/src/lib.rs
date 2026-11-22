//! Demonstration capability: prepare one greeting and execute it through an
//! injected client. This package knows no concrete client, ingress or transport.
//! Registration is effect-free trusted code; construction and task calls are
//! separate. Everything is synchronous and single-threaded.

mod messages;

pub use messages::{
    register, ConfigError, Delivery, DeliveryError, GreetingError, GreetingFactory, GreetingInput,
    GreetingRecipe, GreetingTask, MessageFactories, MessagesConfig, PreparedMessage,
    TaskBuildError, TaskConstruction, TaskProbe, TaskSnapshot, CLIENT_BINDING, GROUP_ID,
    TASK_BINDING,
};

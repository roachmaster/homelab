//! Local llama.cpp lifecycle and typed Forge command bindings.
mod driver;
mod llama;
mod model_runtime;

pub use driver::Driver;
pub use llama::{
    backend_address, register_llama, register_routes, Answer, Chat, Choice, Completion, Config,
    Error, LlamaFactory, MailExpertPaging, MailPromptProfile, Message, Model, Models, Operation,
    Prompt, PromptTokens, Registration, Reply, Request, Role, RuntimeStatus, Timings, Usage,
    MAIL_MODEL_ID,
};

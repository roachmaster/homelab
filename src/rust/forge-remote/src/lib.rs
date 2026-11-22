//! Synchronous, typed node-info conversation. No network, serialization or TLS.
//!
//! Destination callers are trusted adapters: authenticating a peer is their job.
//! The destination independently authorizes the verified subject before dispatch.

mod remote;

pub use remote::{
    now_unix_ms, valid_token, validate_reply, Authorize, Destination, NodeCall, NodeCommand,
    NodeInfo, NodeInfoSource, NodeReply, ReadNodeInfo, ReadNodeInput, ReadNodeTask, RemoteError,
    RemoteNodeClient, Target, CONTRACT_VERSION, MAX_TOKEN_BYTES, NODE_INFO,
};

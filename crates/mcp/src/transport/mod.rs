//! Transport implementations for MCP.
//!
//! REQUIEM supports two transports:
//!
//! * [`stdio`] — launch a child process and speak JSON-RPC over its
//!   stdin/stdout. This is the most common transport for local MCP
//!   servers (Ghidra, IDA, filesystem, …).
//! * [`http`] — connect to a Streamable HTTP endpoint. Used for
//!   remote MCP servers or server deployments that expose an HTTP API.

pub mod http;
pub mod stdio;

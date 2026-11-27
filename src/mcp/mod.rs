//! Model Context Protocol (MCP) types, secret redaction, skill output, CLI secret store, and stdio client.
//!
//! This module provides the transport-neutral JSON-RPC 2.0 and MCP `2025-03-26`
//! protocol framing, serialization-layer secret redaction, embedded operator
//! skill text, `keeppeek secret` CLI helpers, and the `keeppeek mcp` stdio bridge CLI.

pub mod protocol;
pub mod redaction;
pub mod secret_cli;
pub mod skill;
pub mod stdio;

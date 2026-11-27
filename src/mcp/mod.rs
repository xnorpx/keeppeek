//! Model Context Protocol (MCP) types, secret redaction, skill output, and stdio client.
//!
//! This module provides the transport-neutral JSON-RPC 2.0 and MCP `2025-03-26`
//! protocol framing, serialization-layer secret redaction, embedded operator
//! skill text, and the `keeppeek mcp` stdio bridge CLI.

pub mod protocol;
pub mod redaction;
pub mod skill;
pub mod stdio;

//! `nexus-mcp` — a local MCP server exposing NexusView timelines to LLM agents
//! (RF-08).
//!
//! Transport: newline-delimited JSON-RPC 2.0 over stdio, the standard MCP stdio
//! transport. Launch it from an MCP client (e.g. Claude Desktop) or pipe JSON
//! messages to it directly:
//!
//! ```text
//! echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | nexus-mcp
//! ```
//!
//! All data work is delegated to `nexus-core`; this binary only frames messages.

mod server;

use server::Server;
use std::io::{self, BufRead, Write};

fn main() {
    let mut server = Server::new();
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();

    eprintln!("nexus-mcp ready — MCP (JSON-RPC 2.0) over stdio");

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF: client disconnected
            Ok(_) => {
                let message = line.trim();
                if message.is_empty() {
                    continue;
                }
                if let Some(response) = server.handle_line(message) {
                    if writeln!(out, "{response}").is_err() {
                        break;
                    }
                    let _ = out.flush();
                }
            }
            Err(_) => break,
        }
    }
}

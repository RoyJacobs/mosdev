mod diagnostics;
mod highlighting;
mod traits;

use crate::errors::{MosError, MosResult};
use crate::lsp::diagnostics::{DidChange, DidOpen};
use crate::lsp::highlighting::FullRequest;
use lsp_server::{Connection, IoThreads, Message, RequestId};
use lsp_types::notification::Notification;
use lsp_types::{InitializeParams, ServerCapabilities, TextDocumentSyncKind, Url};
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::parser::{Located, Token};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::PathBuf;
pub use traits::*;

pub struct ParsedAst<'a> {
    path: PathBuf,
    source: String,
    ast: Vec<Located<'a, Token<'a>>>,
    error: Option<MosError>,
}

pub struct LspContext<'a> {
    connection: Arc<Connection>,
    io_threads: IoThreads,
    documents: HashMap<Url, Box<ParsedAst<'a>>>,
}

impl<'a> LspContext<'a> {
    fn new() -> Self {
        let (connection, io_threads) = Connection::stdio();

        Self {
            connection: Arc::new(connection),
            io_threads,
            documents: HashMap::new(),
        }
    }

    fn publish_notification<N: Notification>(&self, params: N::Params) -> MosResult<()> {
        let params = serde_json::to_value(&params).unwrap();
        let n = lsp_server::Notification {
            method: N::METHOD.into(),
            params,
        };
        self.connection.sender.send(Message::Notification(n))?;
        Ok(())
    }

    fn send_response<R: DeserializeOwned + Serialize>(
        &self,
        id: RequestId,
        result: R,
    ) -> MosResult<()> {
        let result = serde_json::to_value(&result).unwrap();
        let response = lsp_server::Response {
            id,
            result: Some(result),
            error: None,
        };
        self.connection.sender.send(Message::Response(response))?;
        Ok(())
    }

    fn join(self) -> MosResult<()> {
        self.io_threads.join()?;
        Ok(())
    }
}

pub struct LspServer<'a> {
    context: LspContext<'a>,
    request_handlers: HashMap<&'static str, Box<dyn UntypedRequestHandler>>,
    notification_handlers: HashMap<&'static str, Box<dyn UntypedNotificationHandler>>,
}

impl<'a> LspServer<'a> {
    pub fn new() -> Self {
        let request_handlers = HashMap::new();
        let notification_handlers = HashMap::new();

        Self {
            context: LspContext::new(),
            request_handlers,
            notification_handlers,
        }
    }

    pub fn register_handlers(&mut self) {
        self.register_request_handler(FullRequest {});
        self.register_notification_handler(DidOpen {});
        self.register_notification_handler(DidChange {});
    }

    fn register_request_handler<T: 'static + UntypedRequestHandler>(&mut self, handler: T) {
        self.request_handlers
            .insert(handler.method(), Box::new(handler));
    }

    fn register_notification_handler<T: 'static + UntypedNotificationHandler>(
        &mut self,
        handler: T,
    ) {
        self.notification_handlers
            .insert(handler.method(), Box::new(handler));
    }

    pub fn start(mut self) -> MosResult<()> {
        eprintln!("Starting MOS language server");

        let caps = ServerCapabilities {
            semantic_tokens_provider: Some(highlighting::caps().into()),
            text_document_sync: Some(TextDocumentSyncKind::Full.into()),
            ..Default::default()
        };
        let server_capabilities = serde_json::to_value(&caps).unwrap();
        let initialization_params = self.context.connection.initialize(server_capabilities)?;
        self.main_loop(initialization_params)?;
        self.context.join()?;

        eprintln!("Shutting down MOS language server");
        Ok(())
    }

    fn main_loop(&mut self, params: serde_json::Value) -> MosResult<()> {
        let _params: InitializeParams = serde_json::from_value(params).unwrap();
        for msg in &self.context.connection.clone().receiver {
            //eprintln!("got msg: {:?}", msg);
            match msg {
                Message::Request(req) => {
                    if self.context.connection.handle_shutdown(&req)? {
                        return Ok(());
                    }
                    match self.request_handlers.get(req.method.as_str()) {
                        Some(handler) => {
                            handler.handle(&mut self.context, req)?;
                        }
                        None => {
                            eprintln!("unknown request: {:?}", req);
                        }
                    }
                }
                Message::Response(_resp) => {
                    //eprintln!("got response: {:?}", resp);
                }
                Message::Notification(not) => {
                    match self.notification_handlers.get(not.method.as_str()) {
                        Some(handler) => {
                            handler.handle(&mut self.context, not)?;
                        }
                        None => {
                            eprintln!("unknown notification: {:?}", not);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

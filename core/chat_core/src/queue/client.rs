//! The queue server over HTTPS (spec 9.2): `POST <base>/q/<command>` with the `queue_wire` JSON
//! bodies, on the push proxy's pooled client (`push::client::https_client`). Every request is
//! bounded in time and in reply size, and its answer must hold one result per item.
use queue_wire::{CommandName, MAX_BODY, QueueRequest, QueueResponse};
use reqwest::Url;
use serde::Serialize;

use super::{
    AckItem, CreateItem, DeleteItem, ItemResult, QueueError, QueueFuture, QueueServer, RecvItem,
    SendItem,
};
use crate::push::client::https_client;
use crate::{CoreError, ErrorCode};

/// One whole request (connect, send, read the reply).
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct HttpQueueServer {
    root: Url,
    /// The base's path without its trailing `/`; the command paths follow it.
    prefix: String,
    client: reqwest::Client,
}

impl HttpQueueServer {
    /// A client for the server at `base` (an address `queue::queue_url` gave): `https://`, or in
    /// debug builds also `http://`; anything else is `InvalidConfig`.
    pub fn new(base: &str) -> Result<Self, CoreError> {
        let release = !cfg!(debug_assertions);
        let invalid = || CoreError::new(ErrorCode::InvalidConfig, "queue_url");
        crate::push::check_scheme(base, !release).map_err(|_| invalid())?;
        let root = Url::parse(base).map_err(|_| invalid())?;
        if root.cannot_be_a_base() || root.query().is_some() || root.fragment().is_some() {
            return Err(invalid());
        }
        let prefix = root.path().trim_end_matches('/').to_owned();
        Ok(Self {
            root,
            prefix,
            client: https_client(release)?,
        })
    }

    /// `<base>/q/<command>`, keeping a path the base has.
    fn url(&self, command: CommandName) -> Url {
        let mut url = self.root.clone();
        url.set_path(&format!("{}{}", self.prefix, command.path()));
        url
    }

    fn call<T: Serialize + Send + 'static>(
        &self,
        command: CommandName,
        items: Vec<T>,
    ) -> QueueFuture {
        Box::pin(post(self.client.clone(), self.url(command), items))
    }
}

impl QueueServer for HttpQueueServer {
    fn create(&self, items: Vec<CreateItem>) -> QueueFuture {
        self.call(CommandName::Create, items)
    }
    fn send(&self, items: Vec<SendItem>) -> QueueFuture {
        self.call(CommandName::Send, items)
    }
    fn recv(&self, items: Vec<RecvItem>) -> QueueFuture {
        self.call(CommandName::Recv, items)
    }
    fn ack(&self, items: Vec<AckItem>) -> QueueFuture {
        self.call(CommandName::Ack, items)
    }
    fn delete(&self, items: Vec<DeleteItem>) -> QueueFuture {
        self.call(CommandName::Delete, items)
    }
}

async fn post<T: Serialize>(
    client: reqwest::Client,
    url: Url,
    items: Vec<T>,
) -> Result<Vec<ItemResult>, QueueError> {
    let count = items.len();
    let body = serde_json::to_vec(&QueueRequest { items }).map_err(|_| QueueError::Malformed)?;
    let work = async {
        let mut response = client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(failure)?;
        let status = response.status().as_u16();
        let mut reply = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failure)? {
            if reply.len().saturating_add(chunk.len()) > MAX_BODY {
                return Err(QueueError::Malformed);
            }
            reply.extend_from_slice(&chunk);
        }
        if status != 200 {
            return Err(QueueError::Status(status));
        }
        let response: QueueResponse =
            serde_json::from_slice(&reply).map_err(|_| QueueError::Malformed)?;
        // An answer that does not match the items says nothing about any of them.
        if response.results.len() != count {
            return Err(QueueError::Malformed);
        }
        Ok(response.results)
    };
    tokio::time::timeout(REQUEST_TIMEOUT, work)
        .await
        .unwrap_or(Err(QueueError::Timeout))
}

fn failure(error: reqwest::Error) -> QueueError {
    if error.is_timeout() {
        QueueError::Timeout
    } else {
        QueueError::Network
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_paths_keep_the_base_path() {
        let server = HttpQueueServer::new("http://127.0.0.1:8788").unwrap();
        assert_eq!(
            server.url(CommandName::Create).as_str(),
            "http://127.0.0.1:8788/q/create"
        );
        let server = HttpQueueServer::new("https://host.example/base/").unwrap();
        assert_eq!(
            server.url(CommandName::Delete).as_str(),
            "https://host.example/base/q/delete"
        );
        for bad in ["ftp://x.example", "https://x.example/?q=1", "nonsense"] {
            assert!(HttpQueueServer::new(bad).is_err());
        }
    }
}

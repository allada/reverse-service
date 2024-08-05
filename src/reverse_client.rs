// Copyright 2024 All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::error::Error;

use bytes::Bytes;
use futures::future::{select, Either};
use http::{request::Builder as RequestBuilder, StatusCode};
use http_body_util::Empty;
use hyper::body::{Body, Incoming};
use hyper::header::UPGRADE;
use hyper::rt::bounds::Http2ServerConnExec;
use hyper::rt::{Read, Write};
use hyper::service::HttpService;
use hyper::upgrade::{on as on_http_upgrade, Upgraded};
use hyper::client::conn::http1::handshake as http1_handshake;
use hyper::server::conn::http2::Connection as Http2ServerConnection;

use crate::UPGRADE_HEADER_VALUE;

/// Connect to an http1 endpoint and upgrade the connection,
/// then call the given handler function with the upgraded
/// connection and wait for the handler to finish.
///
/// This is a convenience function that ensures the connection
/// is being processed by a http2 service by the handler.
///
/// # Errors
///
/// This function will return an error if the connection fails to
/// be established, the upgrade fails, or the connection is lost
/// during the upgrade process.
#[allow(clippy::missing_inline_in_public_items)]
pub async fn client_into_service<Io, Handler, Conn, I, B, S, E>(
    io: Io,
    builder: RequestBuilder,
    handler: Handler,
) -> Result<(), Box<dyn Error>>
where
    Io: Write + Read + Send + Unpin + 'static,
    Handler: FnOnce(Upgraded) -> Http2ServerConnection<I, S, E> + Send,
    S: HttpService<Incoming, ResBody = B> + Send,
    S::Error: Into<Box<dyn Error + Send + Sync>> + Send,
    I: Read + Write + Unpin + Send,
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn Error + Send + Sync>> + Send,
    E: Http2ServerConnExec<S::Future, B> + Send,
{
    let upgraded = get_conn_after_upgrade(io, builder).await?;
    handler(upgraded).await.map_err(Into::into)
}

/// Establishes a connection to an http1 endpoint and then upgrades
/// the connection then returns the upgraded connection.
///
/// # Errors
///
/// This function will return an error if the connection fails to
/// be established, the upgrade fails, or the connection is lost
/// during the upgrade process.
#[allow(clippy::missing_inline_in_public_items)]
pub async fn get_conn_after_upgrade<Io>(
    io: Io,
    builder: RequestBuilder,
) -> Result<Upgraded, Box<dyn Error>>
where
    Io: Write + Read + Send + Unpin + 'static,
{
    let request = builder
        .header(UPGRADE, UPGRADE_HEADER_VALUE)
        .body(Empty::<Bytes>::new())?;

    let (mut sender, conn) = http1_handshake(io).await?;

    let request_fut = sender.send_request(request);
    let upgrade_conn = conn.with_upgrades();
    futures::pin_mut!(request_fut);
    futures::pin_mut!(upgrade_conn);
    let upgrade_fut = match select(request_fut, &mut upgrade_conn).await {
        Either::Left((response_res, _)) => {
            let res = response_res?;
            if res.status() != StatusCode::SWITCHING_PROTOCOLS {
                return Err(format!(
                    "Failed to upgrade. Expected {}, got {}",
                    StatusCode::SWITCHING_PROTOCOLS,
                    res.status()
                )
                .into());
            }
            on_http_upgrade(res)
        }
        Either::Right((conn_res, _)) => {
            conn_res?;
            return Err("Disconnected during http upgrade 1".into());
        }
    };

    let upgraded = match select(upgrade_fut, &mut upgrade_conn).await {
        Either::Left((upgrade_res, _)) => upgrade_res?,
        Either::Right((conn_res, _)) => {
            conn_res?;
            return Err("Disconnected during http upgrade 2".into());
        }
    };

    // Although it is not necessary to await the connection, we do
    // just for good measure.
    upgrade_conn.await?;

    Ok(upgraded)
}

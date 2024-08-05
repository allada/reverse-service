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

// #![deny(warnings)]

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::error::Error;

use bytes::Bytes;
use futures::{ready, TryFutureExt};
use http_body_util::Empty;
use hyper::body::Incoming;
use hyper::client::conn::http2::Builder as Http2Builder;
use hyper::client::conn::http2::{Connection as ClientConnection, SendRequest};
use hyper::header::{HeaderValue, UPGRADE};
use hyper::rt::bounds::Http2ClientConnExec;
use hyper::upgrade::on as on_http_upgrade;
use hyper::upgrade::{OnUpgrade, Upgraded};
use hyper::{Request, Response, StatusCode};
use pin_project::pin_project;
use tonic::body::BoxBody;
use tower_service::Service;

use crate::UPGRADE_HEADER_VALUE;

/// A service that when a request is received will initiate
/// an HTTP upgrade request and try to become a client.
///
/// Example use case is a GRPC service that wants has nodes
/// connect to it, but wants to be considered the connecting
/// node to be considered the "service" and the server side
/// of the connection be considered the "client".
pub struct RootService<Ex, H> {
    /// The hyper h2 client builder.
    builder: Http2Builder<Ex>,

    /// The handler to execute when a new client service has
    /// performed the handshake and upgrade and ready to
    /// be used.
    client_handler: H,
}

impl<Ex, H> RootService<Ex, H> {
    /// Create a new `RootService` with a given hyper h2 builder
    /// and a client handler function to handle the new client
    /// service that is created after the handshake and upgrade.
    #[inline]
    pub const fn new(h2_client_builder: Http2Builder<Ex>, client_handler: H) -> Self
    where
        H: Fn(ClientService) + Clone,
    {
        Self {
            builder: h2_client_builder,
            client_handler,
        }
    }
}

impl<Ex, H> Service<Request<Incoming>> for RootService<Ex, H>
where
    Ex: Http2ClientConnExec<BoxBody, Upgraded> + Clone + Unpin + 'static,
    H: Fn(ClientService) + Clone,
{
    type Response = Response<Empty<Bytes>>;
    type Error = Box<dyn Error + Send + Sync>;
    type Future = RootServiceFuture<Ex, H>;

    #[inline]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[inline]
    fn call(&mut self, req: Request<Incoming>) -> Self::Future {
        RootServiceFuture {
            state: RootServiceState::Init(req),
            builder: self.builder.clone(),
            client_handler: self.client_handler.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ClientService(SendRequest<BoxBody>);

impl Service<Request<BoxBody>> for ClientService {
    type Response = Response<Incoming>;
    type Error = Box<dyn Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + 'static>>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx).map_err(Into::into)
    }

    #[inline]
    fn call(&mut self, req: Request<BoxBody>) -> Self::Future {
        Box::pin(self.0.send_request(req).map_err(Into::into))
    }
}

/// The result of the handshake.
type HandshakeResult<Ex, B = BoxBody> =
    Result<(SendRequest<B>, ClientConnection<Upgraded, B, Ex>), hyper::Error>;

/// The state of the future for the upgrade flow.
#[pin_project(project = RootServiceStateProj)]
enum RootServiceState<Ex>
where
    Ex: Http2ClientConnExec<BoxBody, Upgraded> + Clone + Unpin,
{
    /// The initial state of the reverse service.
    Init(Request<Incoming>),

    /// Waiting for the upgrade to complete.
    WaitingForUpgrade(#[pin] OnUpgrade),

    /// The handshake is being performed.
    Handshake(Pin<Box<dyn Future<Output = HandshakeResult<Ex>>>>),

    /// The connection is established and polling the
    /// underlying socket connection to completion.
    Conn(#[pin] ClientConnection<Upgraded, BoxBody, Ex>),
}

#[pin_project]
pub struct RootServiceFuture<Ex, H>
where
    Ex: Http2ClientConnExec<BoxBody, Upgraded> + Clone + Unpin,
{
    /// The current state of the reverse service.
    #[pin]
    state: RootServiceState<Ex>,

    /// The hyper h2 client builder.
    builder: Http2Builder<Ex>,

    /// The handler to execute when a new client service has
    /// performed the handshake and upgrade and ready to
    /// be used.
    client_handler: H,
}

impl<Ex, H> Future for RootServiceFuture<Ex, H>
where
    Ex: Http2ClientConnExec<BoxBody, Upgraded> + Clone + Unpin + 'static,
    H: Fn(ClientService) + Clone,
{
    type Output = Result<Response<Empty<Bytes>>, Box<(dyn Error + Send + Sync + 'static)>>;

    #[inline]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.as_mut().project();
        let state = this.state.as_mut().project();
        match state {
            RootServiceStateProj::Init(request) => {
                let mut response = Response::new(Empty::new());
                if request.headers().get(UPGRADE)
                    != Some(&HeaderValue::from_static(UPGRADE_HEADER_VALUE))
                {
                    *response.status_mut() = StatusCode::BAD_REQUEST;
                    return Poll::Ready(Ok(response));
                }
                let upgraded_fut = on_http_upgrade(request);
                *this.state = RootServiceState::WaitingForUpgrade(upgraded_fut);

                *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
                response
                    .headers_mut()
                    .insert(UPGRADE, HeaderValue::from_static(UPGRADE_HEADER_VALUE));
                Poll::Ready(Ok(response))
            }
            RootServiceStateProj::WaitingForUpgrade(upgraded_fut) => {
                let upgraded = ready!(upgraded_fut.poll(cx))?;
                // self.state = RootServiceState::Upgraded{ upgraded };
                let handshake = Box::pin(this.builder.handshake(upgraded));
                *this.state = RootServiceState::Handshake(handshake);
                self.poll(cx)
            }
            RootServiceStateProj::Handshake(handshake) => {
                let (send_request, conn) = ready!(handshake.as_mut().poll(cx))?;
                (this.client_handler)(ClientService(send_request));
                *this.state = RootServiceState::Conn(conn);
                self.poll(cx)
            }
            RootServiceStateProj::Conn(conn) => match conn.poll(cx) {
                Poll::Ready(Ok(())) => Poll::Ready(Ok(Response::new(Empty::new()))),
                Poll::Ready(Err(err)) => Poll::Ready(Err(Box::new(err))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

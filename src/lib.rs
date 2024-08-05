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

//! This library takes a normal HTTP connection and will attempt to
//! convert the server to a client and the client to a server. This
//! is done by initiating an HTTP upgrade request during the inital
//! HTTP1 request and then requesting the server to upgrade to a
//! custom protocol that is defined by `UPGRADE_HEADER_VALUE`. When
//! performed correctly the original-server will then take the
//! underlying connection (usually TCP) and start a new handshake
//! with the original-client, but in the reverse roles.
//!
//! This can be very useful for GRPC services that want to allow
//! a group of nodes to initiate connections to a single node, but
//! the single node is going to call GRPC functions on the connecting
//! nodes (group of nodes).

#![deny(
    clippy::all,
    clippy::restriction,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo
)]
#![allow(
    clippy::implicit_return,
    clippy::question_mark_used,
    clippy::blanket_clippy_restriction_lints
)]

pub mod reverse_service;

/// The value of the `upgrade` header that is used to signal
/// that the connection should be upgraded.
const UPGRADE_HEADER_VALUE: &str = "reverse-upgrade";

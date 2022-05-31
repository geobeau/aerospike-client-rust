// Copyright 2015-2018 Aerospike, Inc.
//
// Portions may be licensed to Aerospike, Inc. under one or more contributor
// license agreements.
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not
// use this file except in compliance with the License. You may obtain a copy of
// the License at http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
// WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
// License for the specific language governing permissions and limitations under
// the License.

use rustls::{ClientConnection, StreamOwned};
use std::convert::TryInto;
use std::io::prelude::*;
use std::io::Write;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::ops::Add;
use std::time::{Duration, Instant};

use crate::commands::admin_command::AdminCommand;
use crate::commands::buffer::Buffer;
use crate::errors::Result;
use crate::policy::ClientPolicy;

#[derive(Debug)]
pub struct Connection {
    timeout: Option<Duration>,

    // duration after which connection is considered idle
    idle_timeout: Option<Duration>,
    idle_deadline: Option<Instant>,

    // connection object
    conn: Stream,

    bytes_read: usize,

    pub buffer: Buffer,
}

#[derive(Debug)]
enum Stream {
    TLS(StreamOwned<ClientConnection, TcpStream>),
    Plain(TcpStream),
}

impl Connection {
    pub fn new<T: ToSocketAddrs + Copy>(addr: T, policy: &ClientPolicy) -> Result<Self> {
        let tcp_stream = TcpStream::connect(addr)?;
        let stream = match &policy.tls_config {
            Some(config) => {
                let server_name = "aerospike.preprod.crto.in".try_into().unwrap();
                let conn = ClientConnection::new(config.clone(), server_name);
                let sock = TcpStream::connect(addr)?;
                let tls = StreamOwned::new(conn.unwrap(), sock);
                Stream::TLS(tls)
            }
            None => Stream::Plain(tcp_stream),
        };

        let mut conn = Connection {
            buffer: Buffer::new(policy.buffer_reclaim_threshold),
            bytes_read: 0,
            timeout: policy.timeout,
            conn: stream,
            idle_timeout: policy.idle_timeout,
            idle_deadline: match policy.idle_timeout {
                None => None,
                Some(timeout) => Some(Instant::now() + timeout),
            },
        };
        conn.authenticate(&policy.user_password)?;
        conn.refresh();
        Ok(conn)
    }

    pub fn close(&mut self) {
        match &self.conn {
            Stream::TLS(s) => {
                let _ = s.sock.shutdown(Shutdown::Both);
            }
            Stream::Plain(s) => {
                let _ = s.shutdown(Shutdown::Both);
            }
        };
    }

    pub fn flush(&mut self) -> Result<()> {
        match &mut self.conn {
            Stream::TLS(s) => s.write_all(&self.buffer.data_buffer)?,
            Stream::Plain(s) => s.write_all(&self.buffer.data_buffer)?,
        };

        self.refresh();
        Ok(())
    }

    pub fn read_buffer(&mut self, size: usize) -> Result<()> {
        self.buffer.resize_buffer(size)?;
        match &mut self.conn {
            Stream::TLS(s) => s.read_exact(&mut self.buffer.data_buffer)?,
            Stream::Plain(s) => s.read_exact(&mut self.buffer.data_buffer)?,
        };
        self.bytes_read += size;
        self.buffer.reset_offset()?;
        self.refresh();
        Ok(())
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<()> {
        match &mut self.conn {
            Stream::TLS(s) => s.write_all(buf)?,
            Stream::Plain(s) => s.write_all(buf)?,
        };
        self.refresh();
        Ok(())
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<()> {
        match &mut self.conn {
            Stream::TLS(s) => s.read_exact(buf)?,
            Stream::Plain(s) => s.read_exact(buf)?,
        };
        self.bytes_read += buf.len();
        self.refresh();
        Ok(())
    }

    pub fn set_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        match &self.conn {
            Stream::TLS(s) => {
                s.sock.set_read_timeout(timeout)?;
                s.sock.set_write_timeout(timeout)?;
            }
            Stream::Plain(s) => {
                s.set_read_timeout(timeout)?;
                s.set_write_timeout(timeout)?;
            }
        };

        Ok(())
    }

    pub fn is_idle(&self) -> bool {
        self.idle_deadline
            .map_or(false, |idle_dl| Instant::now() >= idle_dl)
    }

    fn refresh(&mut self) {
        self.idle_deadline = None;
        if let Some(idle_to) = self.idle_timeout {
            self.idle_deadline = Some(Instant::now().add(idle_to))
        };
    }

    fn authenticate(&mut self, user_password: &Option<(String, String)>) -> Result<()> {
        if let Some((ref user, ref password)) = *user_password {
            match AdminCommand::authenticate(self, user, password) {
                Ok(()) => {
                    return Ok(());
                }
                Err(err) => {
                    self.close();
                    return Err(err);
                }
            }
        }

        Ok(())
    }

    pub fn bookmark(&mut self) {
        self.bytes_read = 0;
    }

    pub const fn bytes_read(&self) -> usize {
        self.bytes_read
    }
}

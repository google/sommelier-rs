/*
Copyright 2026 Google LLC

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

     https://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

use std::convert::TryInto;
use std::fmt;
use std::os::unix::io::RawFd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Forward,
    Drop,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InsufficientData,
    InvalidString,
    MissingFd,
    UnknownOpcode(u16),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::InsufficientData => write!(f, "Insufficient data in wire message"),
            ProtocolError::InvalidString => write!(f, "Invalid string in wire message"),
            ProtocolError::MissingFd => write!(f, "Missing file descriptor in wire message"),
            ProtocolError::UnknownOpcode(opcode) => write!(f, "Unknown opcode: {}", opcode),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Debug)]
pub struct WireMessage<'a> {
    pub sender_id: u32,
    pub opcode: u16,
    pub payload: &'a [u8],
    pub fds: &'a [RawFd],
    pub offset: usize,
    pub fd_offset: usize,
}

impl<'a> WireMessage<'a> {
    pub fn new(sender_id: u32, opcode: u16, payload: &'a [u8], fds: &'a [RawFd]) -> Self {
        Self {
            sender_id,
            opcode,
            payload,
            fds,
            offset: 0,
            fd_offset: 0,
        }
    }

    pub fn read_u32(&mut self) -> Result<u32, ProtocolError> {
        if self.offset + 4 > self.payload.len() {
            return Err(ProtocolError::InsufficientData);
        }
        let bytes = &self.payload[self.offset..self.offset + 4];
        self.offset += 4;
        Ok(u32::from_ne_bytes(bytes.try_into().unwrap()))
    }

    pub fn read_i32(&mut self) -> Result<i32, ProtocolError> {
        self.read_u32().map(|v| v as i32)
    }

    pub fn read_fixed(&mut self) -> Result<f32, ProtocolError> {
        // Wayland fixed is 24.8 signed fixed point
        let v = self.read_i32()?;
        Ok((v as f32) / 256.0)
    }

    pub fn read_string(&mut self) -> Result<String, ProtocolError> {
        let len = self.read_u32()? as usize;
        if len == 0 {
            return Ok(String::new());
        }
        // Strings are padded to 32-bit boundary
        let padded_len = (len + 3) & !3;

        if self.offset + padded_len > self.payload.len() {
            return Err(ProtocolError::InsufficientData);
        }

        let bytes = &self.payload[self.offset..self.offset + len];

        // Wayland strings include null terminator in the length.
        // We expect at least one byte for the null terminator if len > 0.
        if bytes.last() != Some(&0) {
            return Err(ProtocolError::InvalidString);
        }

        // To match C behavior, we truncate at the first null terminator if there are interior nulls
        let null_pos = bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(len.saturating_sub(1));
        let s = String::from_utf8_lossy(&bytes[..null_pos]).into_owned();

        self.offset += padded_len;
        Ok(s)
    }

    pub fn read_array(&mut self) -> Result<Vec<u8>, ProtocolError> {
        let len = self.read_u32()? as usize;
        let padded_len = (len + 3) & !3;

        if self.offset + padded_len > self.payload.len() {
            return Err(ProtocolError::InsufficientData);
        }

        let bytes = self.payload[self.offset..self.offset + len].to_vec();
        self.offset += padded_len;
        Ok(bytes)
    }

    pub fn read_fd(&mut self) -> Result<RawFd, ProtocolError> {
        if self.fd_offset < self.fds.len() {
            let fd = self.fds[self.fd_offset];
            self.fd_offset += 1;
            Ok(fd)
        } else {
            Err(ProtocolError::MissingFd)
        }
    }
}

pub struct MessageBuilder {
    pub payload: Vec<u8>,
    pub fds: Vec<RawFd>,
}

impl MessageBuilder {
    pub fn new() -> Self {
        Self {
            payload: Vec::new(),
            fds: Vec::new(),
        }
    }

    pub fn write_u32(&mut self, val: u32) {
        self.payload.extend_from_slice(&val.to_ne_bytes());
    }

    pub fn write_i32(&mut self, val: i32) {
        self.write_u32(val as u32);
    }

    pub fn write_fixed(&mut self, val: f32) {
        let v = (val * 256.0) as i32;
        self.write_i32(v);
    }

    pub fn write_string(&mut self, val: &str) {
        let len = val.len() as u32 + 1; // +1 for null terminator
        self.write_u32(len);

        self.payload.extend_from_slice(val.as_bytes());
        self.payload.push(0); // null terminator

        // padding
        let padded_len = (len + 3) & !3;
        let padding = padded_len - len;
        self.payload
            .resize(self.payload.len() + padding as usize, 0);
    }

    pub fn write_array(&mut self, val: &[u8]) {
        let len = val.len() as u32;
        self.write_u32(len);

        self.payload.extend_from_slice(val);

        let padded_len = (len + 3) & !3;
        let padding = padded_len - len;
        self.payload
            .resize(self.payload.len() + padding as usize, 0);
    }

    pub fn write_fd(&mut self, fd: RawFd) {
        self.fds.push(fd);
    }
}

impl Default for MessageBuilder {
    fn default() -> Self {
        Self::new()
    }
}

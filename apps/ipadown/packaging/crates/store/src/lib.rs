//! ipadown-store: App Store 搜索、ipatool 进程封装与 IPA 元数据。
//!
//! 协议实现参考一手源码：
//! - majd/ipatool (Go): https://github.com/majd/ipatool

pub mod apple_edge;
pub mod config;
mod countries;
mod error;
pub mod history;
mod http;
pub mod ipa;
pub mod ipatool;
mod jwe;
pub mod logging;
mod models;
pub mod search;

pub use error::{Result, StoreError};
pub use models::Software;

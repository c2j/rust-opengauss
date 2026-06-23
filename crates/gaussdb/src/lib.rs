//! 统一的 openGauss/PostgreSQL 客户端入口。
//!
//! # 异步与同步
//!
//! **`gaussdb::Client` 是异步的**(默认,在 crate 根)。方法返回 Future,需 `.await`。
//!
//! 若需同步 API,启用 `sync` feature 并使用 `gaussdb::sync::Client`。
//!
//! # ⚠️ 重要:不要同时 glob-import 根与 sync 模块
//!
//! `use gaussdb::*; use gaussdb::sync::*;` 会**静默遮蔽**根的异步类型
//! (`Client`/`Row`/`Error`/`Config`/`NoTls` 等),导致你拿到同步类型却在写 async 代码。
//! 请显式 import 或只用其一。
//!
//! # SemVer 耦合
//!
//! `gaussdb` 0.x.y 重新导出 `tokio-opengauss`。tokio-opengauss 或 opengauss-types
//! 的破坏性变更 ⇒ gaussdb 破坏性 bump。

pub use fallible_iterator;

// === 异步表面(主,crate 根)===
#[cfg(feature = "runtime")]
pub use tokio_opengauss::connect;
pub use tokio_opengauss::{
    AsyncMessage, CancelToken, Client, Column, Config, Connection, CopyInSink, CopyOutStream,
    Error, GenericClient, IsolationLevel, NoTls, Notification, Portal, Row, RowStream,
    SimpleColumn, SimpleQueryMessage, SimpleQueryRow, SimpleQueryStream, Socket, Statement,
    ToStatement, Transaction, TransactionBuilder, binary_copy, config, error, row, tls, types,
};

// === 同步表面(opt-in)===
#[cfg(feature = "sync")]
pub mod sync {
    //! 同步客户端。这些类型与 crate 根的异步类型**同名但不同类型**。
    //! 不要 `use gaussdb::sync::*` 同时又 `use gaussdb::*`。

    pub use opengauss::{
        CancelToken, Client, Config, CopyInWriter, CopyOutReader, Error, GenericClient, NoTls,
        Notifications, Row, RowIter, SimpleQueryRow, Transaction, TransactionBuilder, binary_copy,
        config, notifications,
    };

    /// 同步连接便捷函数(与根 `gaussdb::connect` 对称)。
    pub fn connect<T>(params: &str, tls: T) -> Result<Client, Error>
    where
        T: tokio_opengauss::tls::MakeTlsConnect<tokio_opengauss::Socket> + 'static + Send,
        T::TlsConnect: Send,
        T::Stream: Send,
        <T::TlsConnect as tokio_opengauss::tls::TlsConnect<tokio_opengauss::Socket>>::Future: Send,
    {
        Client::connect(params, tls)
    }
}

// === TLS(命名空间隔离)===
#[cfg(feature = "tls-native-tls")]
pub use opengauss_native_tls as native_tls;
#[cfg(feature = "tls-openssl")]
pub use opengauss_openssl as openssl;

// === 协议(最小暴露)===
pub use opengauss_protocol::Oid;

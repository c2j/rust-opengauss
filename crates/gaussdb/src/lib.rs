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
//! # 命名空间布局
//!
//! | 路径 | 内容 |
//! |---|---|
//! | crate 根(`gaussdb::*`) | 异步便捷别名(`Client`/`Row`/`Config`/`NoTls`/…) |
//! | `gaussdb::sync::*` | 同步便捷别名(opt-in `sync` feature) |
//! | `gaussdb::driver::*` | 异步低层完整表面(tokio-opengauss 全量,含 `config` 模块) |
//! | `gaussdb::sync::driver::*` | 同步低层完整表面(opengauss 全量,含 `config` 模块) |
//! | `gaussdb::native_tls` / `gaussdb::openssl` | TLS 连接器(feature-gated) |
//!
//! **`gaussdb::config`** 留作高层配置解析模块(待 `config` feature 引入)。
//! 若需低层 driver Config builder 模块,请走 `gaussdb::driver::config`。
//!
//! # SemVer 耦合
//!
//! `gaussdb` 0.x.y 重新导出 `tokio-opengauss`。tokio-opengauss 或 opengauss-types
//! 的破坏性变更 ⇒ gaussdb 破坏性 bump。

pub use fallible_iterator;

pub mod duration_parse;

// === 低层 driver 命名空间(异步,完整表面)===
/// tokio-opengauss 全量 re-export。需要低层 `config` 模块(SslMode 等)时走此路径。
pub mod driver {
    pub use tokio_opengauss::*;
}

// === 异步表面(主,crate 根便捷别名)===
// 注意:故意不 re-export `config` 模块——该命名空间留给高层 gaussdb::config。
#[cfg(feature = "runtime")]
pub use driver::connect;
pub use driver::{
    AsyncMessage, CancelToken, Client, Column, Config, Connection, CopyInSink, CopyOutStream,
    Error, GenericClient, IsolationLevel, NoTls, Notification, Portal, Row, RowStream,
    SimpleColumn, SimpleQueryMessage, SimpleQueryRow, SimpleQueryStream, Socket, Statement,
    ToStatement, Transaction, TransactionBuilder, binary_copy, error, row, tls, types,
};

// === 同步表面(opt-in)===
#[cfg(feature = "sync")]
pub mod sync {
    //! 同步客户端。这些类型与 crate 根的异步类型**同名但不同类型**。
    //! 不要 `use gaussdb::sync::*` 同时又 `use gaussdb::*`。

    /// opengauss(同步)全量 re-export。需要低层 `config` 模块时走 `sync::driver::config`。
    pub mod driver {
        pub use opengauss::*;
    }

    // 便捷别名:故意不 re-export `config` 模块——与根策略一致。
    pub use driver::{
        CancelToken, Client, Config, CopyInWriter, CopyOutReader, Error, GenericClient, NoTls,
        Notifications, Row, RowIter, SimpleQueryRow, Transaction, TransactionBuilder, binary_copy,
        notifications,
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

// === 高层配置解析(opt-in, gated by `config` feature)===
#[cfg(feature = "config")]
pub mod config;

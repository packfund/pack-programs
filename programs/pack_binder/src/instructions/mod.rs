//! One module per group of instructions. Each exports its `Accounts` structs, its argument structs and uniquely named
//! `handle_*` functions, so the glob re-exports below never clash.

pub mod admin;
pub mod crank;
pub mod forfeit;
pub mod init;
pub mod mint;
pub mod reserve;
pub mod vault;

pub use admin::*;
pub use crank::*;
pub use forfeit::*;
pub use init::*;
pub use mint::*;
pub use reserve::*;
pub use vault::*;

//! Built-in Lightspeed CoreAgent domain.
//!
//! This module contains the closed agent composition that Lightspeed ships by
//! default. The lower-level `session` module owns the extensible event-log
//! primitives; CoreAgent is just one domain built on top of them.

pub mod admit;
pub mod apply;
pub mod codec;
pub mod components;
pub mod domain;
pub mod drive;
pub mod io;
pub mod planning;
pub mod session_graph;
pub mod transition;
pub mod workflow;

pub use admit::*;
pub use apply::*;
pub use codec::*;
pub use components::*;
pub use domain::*;
pub use drive::*;
pub use io::*;
pub use planning::*;
pub use session_graph::*;
pub use transition::*;

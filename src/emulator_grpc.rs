//! Generated Android emulator gRPC bindings.
//!
//! ## Rationale
//! Provides the raw tonic-generated proto bindings for the emulator's
//! gRPC control surface.
//!
//! ## Security Boundaries
//! * Pure code generation layer; enforces proto-contract safety.
//!
//! ## References
//! * [Android Emulator gRPC API](https://developer.android.com/studio/run/emulator-console)

// Documentation is emitted from Google's SDK proto, outside this crate's control.
#![allow(clippy::doc_lazy_continuation)]

tonic::include_proto!("android.emulation.control");

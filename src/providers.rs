//! `StreamProvider` trait, registry, and concrete providers (03). Multiprovider
//! is day-1 architecture: N providers with per-provider availability, never a
//! single provider with a seam. Imports domain (+ own http helpers); NEVER tui
//! or store (01 §5, keeps backends testable offline). Filled in ROD-436.

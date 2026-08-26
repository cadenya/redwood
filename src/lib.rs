// Code emitters intentionally compose smaller formatted fragments into large
// language templates. Flattening those expressions makes generated-code
// templates substantially harder to review.
#![allow(clippy::format_in_format_args)]

pub mod backends;
pub mod config;
pub mod input;
pub mod ir;
pub mod openapi;
pub mod output;

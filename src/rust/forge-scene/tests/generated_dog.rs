//! Compiles the rendered dog scene and runs the tests it ships with.
//! The generated file's layout comes from the template, so rustfmt skips it;
//! `render_golden` pins its bytes instead.

#[rustfmt::skip]
#[path = "../generated/dog.rs"]
mod dog;

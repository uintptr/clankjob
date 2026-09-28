# Agent Guidelines for Rust Code Quality

This document provides guidelines for maintaining high-quality Rust code. These rules MUST be followed by all AI coding agents and contributors.

## Your Core Principles

All code you write MUST be fully optimized.

"Fully optimized" includes:

- maximizing algorithmic big-O efficiency for memory and runtime
- using parallelization and SIMD where appropriate
- following proper style conventions for Rust (e.g. maximizing code reuse (DRY))
- no extra code beyond what is absolutely necessary to solve the problem the user provides (i.e. no technical debt)

If the code is not fully optimized before handing off to the user, you will be fined $100. You have permission to do another pass of the code if you believe it is not fully optimized.

## Preferred Tools

- Use `cargo` for project management, building, and dependency management.
- Use `indicatif` to track long-running operations with progress bars. The message should be contextually sensitive.
- Use `serde` with `serde_json` for JSON serialization/deserialization.
- Use `ratatui` adnd `crossterm` for terminal applications/TUIs.
- Use `rouille` for creating any web servers or HTTP APIs.
    - **NEVER** use `async`/`await`, `tokio`, `async-std`, or any other async runtime. The
      code base is synchronous: blocking I/O, `std::thread`, and channels.
    - Pick blocking crates for I/O: `rusqlite` for SQLite, `ureq` for HTTP clients,
      `lettre` (sync transport) for SMTP, `imap` for IMAP. Do not wrap an async crate in
      a `block_on` to make it fit.
    - Keep request handlers small and returning `Result<Response, AppError>` so error
      handling is centralized in one conversion.
    - Pass shared state (DB pool, config, registries) to handlers explicitly through a
      state struct captured by the router closure, never through global mutable data.
    - Handlers must never do long-running work (LLM calls, IMAP, plugin calls). Enqueue
      it for a background worker thread instead, so request threads stay free.
- When reporting errors to the console, use `tracing::error!` or `log::error!` instead of `println!`.
- If designing applications with a web-based front end interface, e.g. compiling to WASM or using `dioxus`:
    - All deep computation **MUST** occur within Rust processes (i.e. the WASM binary or the `dioxus` app Rust process). **NEVER** use JavaScript for deep computation.
    - The front-end **MUST** use Pico CSS and vanilla JavaScript. **NEVER** use jQuery or any component-based frameworks such as React.
    - The front-end should prioritize speed and common HID guidelines.
    - The app should use adaptive light/dark themes by default, with a toggle to switch the themes.
    - The typography/theming of the application **MUST** be modern and unique, similar to that of popular single-page web/mobile. **ALWAYS** add an appropriate font for headers and body text. You may reference fonts from Google Fonts.
    - **NEVER** use the Pico CSS defaults as-is: a separate CSS/SCSS file is encouraged. The design **MUST** logically complement the semantics of the application use case.
    - **ALWAYS** rebuild the WASM binary if any underlying Rust code that affects it is touched.
- For data processing:
    - **ALWAYS** use `polars` instead of other data frame libraries for tabular data manipulation.
    - If a `polars` dataframe will be printed, **NEVER** simultaneously print the number of entries in the dataframe nor the schema as it is redundant.
    - **NEVER** ingest more than 10 rows of a data frame at a time. Only analyze subsets of data to avoid overloading your memory context.
- If using Python to implement Rust code using PyO3/`maturin`:
    - Rebuild the Python package with `maturin` after finishing all Rust code changes.
    - **ALWAYS** use `uv` for Python package management and to create a `.venv` if it is not present. **NEVER** use the base system Python installation.
    - Ensure `.venv` is added to `.gitignore`.
    - Ensure `ipykernel` and `ipywidgets` is installed in `.venv` for Jupyter Notebook compatability. This should not be in package requirements.
    - **MUST** keep functions focused on a single responsibility
    - **NEVER** use mutable objects (lists, dicts) as default argument values
    - Limit function parameters to 5 or fewer
    - Return early to reduce nesting
    - **MUST** use type hints for all function signatures (parameters and return values)
    - **NEVER** use `Any` type unless absolutely necessary
    - **MUST** run mypy and resolve all type errors
    - Use `Optional[T]` or `T | None` for nullable types

## Code Style and Formatting

- **MUST** use meaningful, descriptive variable and function names
- **MUST** follow Rust API Guidelines and idiomatic Rust conventions
- **MUST** use 4 spaces for indentation (never tabs)
- **NEVER** use emoji, or unicode that emulates emoji (e.g. ✓, ✗). The only exception is when writing tests and testing the impact of multibyte characters.
- Use snake_case for functions/variables/modules, PascalCase for types/traits, SCREAMING_SNAKE_CASE for constants
- Limit line length to 100 characters (rustfmt default)
- Assume the user is a Python expert, but a Rust novice. Include additional code comments around Rust-specific nuances that a Python developer may not recognize.
- **MUST** use `where` clauses for generic trait bounds instead of inline bounds in the
  generic parameter list. This keeps the function signature readable and scales cleanly
  when bounds grow.

```rust
// Good
pub fn method<T>(&self, value: T) -> Output
where
    T: Trait,
{
    // ...
}

// Bad
pub fn method<T: Trait>(&self, value: T) -> Output {
    // ...
}
```

## Documentation

- **MUST** include doc comments for all public functions, structs, enums, and methods
- **MUST** document function parameters, return values, and errors
- Keep comments up-to-date with code changes
- Include examples in doc comments for complex functions

Example doc comment:

````rust
/// Compute the result from the provided inputs.
///
/// # Arguments
///
/// * `inputs` - Slice of input values
/// * `scaling` - Scaling factor as a decimal (e.g., 0.08 for 8%)
///
/// # Returns
///
/// The computed result
///
/// # Errors
///
/// Returns `ComputationError::EmptyInputs` if inputs is empty
/// Returns `ComputationError::InvalidScaling` if scaling is negative
///
/// # Examples
///
/// ```
/// let inputs = vec![Input { value: 10.0 }, Input { value: 20.0 }];
/// let result = compute(&inputs, 0.08)?;
/// assert_eq!(result, 32.40);
/// ```
pub fn compute(inputs: &[Input], scaling: f64) -> Result<f64, ComputationError> {
````

## Type System

- **MUST** leverage Rust's type system to prevent bugs at compile time
- **NEVER** use `.unwrap()` in library code; use `.expect()` only for invariant violations with a descriptive message
- **MUST** use meaningful custom error types with `thiserror`
- Use newtypes to distinguish semantically different values of the same underlying type
- Prefer `Option<T>` over sentinel values

## Error Handling

- **NEVER** use `.unwrap()` in production code paths
- **MUST** use `Result<T, E>` for fallible operations
- **MUST** use `thiserror` for defining error types and `anyhow` for application-level errors
- **MUST** propagate errors with `?` operator where appropriate
- Provide meaningful error messages with context using `.context()` from `anyhow`

## Function Design

- **MUST** keep functions focused on a single responsibility
- **MUST** prefer borrowing (`&T`, `&mut T`) over ownership when possible
- **MUST** place private helper functions above the public functions that use them
- Limit function parameters to 5 or fewer; use a config struct for more
- Return early to reduce nesting
- Use iterators and combinators over explicit loops where clearer
- Prefer a small enum over a `bool` parameter when the meaning isn't obvious at the call site. `run(path, Capture::Discard)` reads clearly; `run(path, false)` forces the reader to remember what the boolean controls. The enum also makes it easy to add a third variant later without breaking every caller.

## Public API Flexibility

Public functions that take a string or filesystem path **MUST** accept
generic `AsRef` parameters rather than concrete reference types. This
lets callers pass `&str`, `&String`, `String`, `&Path`, `&PathBuf`,
`PathBuf`, or anything else that already implements the trait — without
forcing them to write `.as_ref()` or `.as_str()` at the call site.

```rust
// Good — accepts &str, &String, String, Cow<str>, ...
pub fn new<S>(value: S) -> Result<Self>
where
    S: AsRef<str>,
{
    let value = value.as_ref();
    ...
}

// Good — accepts &Path, &PathBuf, PathBuf, &str, String, ...
pub fn load<P>(&self, source: P) -> Result<()>
where
    P: AsRef<Path>,
{
    let source = source.as_ref();
    ...
}

// Bad — forces every caller that holds a String/PathBuf to borrow
pub fn new(value: &str) -> Result<Self> { ... }
pub fn load(&self, source: &Path) -> Result<()> { ... }
```

Call `.as_ref()` once at the top of the function and shadow the
parameter with the borrowed view (`let source = source.as_ref();`). This
keeps the body identical to a `&Path` / `&str` implementation while
making the signature ergonomic for callers.

This rule applies to **public** APIs. Private helpers should keep
concrete reference types (`&str`, `&Path`) — the extra generics buy
nothing internally and bloat compile times.

## FFI / cbindgen: typed opaque handles, never `*mut c_void`

`extern "C"` functions that hand a Rust object to C as an opaque handle
**MUST NOT** use `*mut c_void`. Every distinct Rust type gets its own
named opaque wrapper, and the FFI signatures take/return a pointer to
that wrapper. A bare `void *` erases the type, so the C compiler will
silently let a caller pass _any_ handle to _any_ function — and the only
symptom is a memory-corruption crash at runtime (e.g. one handle's bytes
reinterpreted as a different struct, then dereferenced).

With distinct opaque types, cbindgen emits one `typedef struct Foo Foo;`
per handle and the C compiler rejects a mismatched pointer at the call
site (`-Wincompatible-pointer-types`, which is `-Werror` here). The bug
class disappears at compile time instead of becoming a runtime crash.

Define the wrapper in the **capi crate** (not the underlying crate), give
it a single private field holding the real type, and annotate it
`cbindgen:opaque` so cbindgen forward-declares it without trying to emit
the (foreign, non-`repr(C)`) body. Access the inner value through the
field — never cast the pointer.

```rust
// Good — one named opaque handle per type; the compiler enforces them.
//
/// Opaque handle to an inner object, created by `rs_c_object_new`.
///
/// cbindgen:opaque
pub struct RsCObjectHandle {
    inner: Object,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_c_object_new(param: u16) -> *mut RsCObjectHandle {
    // ...
    Box::into_raw(Box::new(RsCObjectHandle { inner }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_c_object_status(
    handle: *mut RsCObjectHandle,
    out: *mut RsCObjectStatus,
) -> i32 {
    if handle.is_null() || out.is_null() {
        return Error::InvalidParameter.into();
    }
    let inner = unsafe { &(*handle).inner };
    // ...
}

// Bad — void* erases the type; passing the wrong handle compiles fine
// and corrupts memory at runtime. The `as`/`.cast()` is the smell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_c_object_status(handle: *mut c_void, out: *mut RsCObjectStatus) -> i32 {
    let inner = unsafe { &*(handle as *const Object) };
    // ...
}
```

The C side must cooperate for the check to bite: type the variables and
struct fields that store the handle as the generated pointer type
(`struct RsCObjectHandle *`), **not** `void *`. A `void *` storage slot
converts implicitly to any pointer, so it silently defeats the typing —
the protection only works when the handle is typed end to end.

## Struct and Enum Design

- **MUST** keep types focused on a single responsibility
- **MUST** derive common traits: `Debug`, `Clone`, `PartialEq` where appropriate
- Use `#[derive(Default)]` when a sensible default exists
- Prefer composition over inheritance-like patterns
- Use builder pattern for complex struct construction
- Make fields private by default; provide accessor methods when needed

## Testing

- **MUST** write unit tests for all new functions and types
- **MUST** mock external dependencies (APIs, databases, file systems)
- **MUST** use the built-in `#[test]` attribute and `cargo test`
- Follow the Arrange-Act-Assert pattern
- Do not commit commented-out tests
- Use `#[cfg(test)]` modules for test code

## Imports and Dependencies

- **MUST** avoid wildcard imports (`use module::*`) except for preludes, test modules (`use super::*`), and prelude re-exports
- **MUST** document dependencies in `Cargo.toml` with version constraints
- **MUST** specify dependency versions without the patch component; use `thiserror = "2.0"` instead of `thiserror = "2.0.21"`. The patch version is resolved by `Cargo.lock` and pinning it in `Cargo.toml` needlessly prevents compatible patch updates.
- Use `cargo` for dependency management
- Organize imports: standard library, external crates, local modules
- Use `rustfmt` to automate import formatting

## Platform Portability

- **MUST** use `std::env::consts` (`EXE_SUFFIX`, `DLL_EXTENSION`, `DLL_PREFIX`, `OS`, `ARCH`)
  instead of `#[cfg]` blocks when building platform-dependent strings like file names.
  Reserve `#[cfg]` for cases where the code itself differs across platforms (e.g. calling
  different OS APIs), not for values that only differ in a suffix or extension.

## Rust Best Practices

- **NEVER** use `unsafe` unless absolutely necessary; document safety invariants when used
- **MUST** call `.clone()` explicitly on non-`Copy` types; avoid hidden clones in closures and iterators
- **MUST** use pattern matching exhaustively; avoid catch-all `_` patterns when possible
- **MUST** use `format!` macro for string formatting
- Use iterators and iterator adapters over manual loops
- Use `enumerate()` instead of manual counter variables
- Prefer `if let` and `while let` for single-pattern matching

## Memory and Performance

- **MUST** avoid unnecessary allocations; prefer `&str` over `String` when possible
- **MUST** use `Cow<'_, str>` when ownership is conditionally needed
- Use `Vec::with_capacity()` when the size is known
- Prefer stack allocation over heap when appropriate
- Use `Arc` and `Rc` judiciously; prefer borrowing

## Concurrency

- **MUST** use `Send` and `Sync` bounds appropriately
- **NEVER** use async code or an async runtime (see Preferred Tools); use threads
- **MUST** use `rayon` for CPU-bound parallelism
- Avoid `Mutex` when `RwLock` or lock-free alternatives are appropriate
- Use channels (`mpsc`, `crossbeam`) for message passing

## Security

- **NEVER** store secrets, API keys, or passwords in code. Only store them in `.env`.
    - Ensure `.env` is declared in `.gitignore`.
- **MUST** use environment variables for sensitive configuration via `dotenvy` or `std::env`
- **NEVER** log sensitive information (passwords, tokens, PII)
- Use `secrecy` crate for sensitive data types

## Version Control

- **MUST** write clear, descriptive commit messages
- **NEVER** commit commented-out code; delete it
- **NEVER** commit debug `println!` statements or `dbg!` macros
- **NEVER** commit credentials or sensitive data

## Tools

- **MUST** use `rustfmt` for code formatting — run `cargo fmt` after writing or modifying Rust code
- **MUST** use `clippy` for linting and follow its suggestions
- **MUST** Not allowed to use #[allow(clippy::indexing_slicing)]
- **MUST** Not allowed to use #[allow(unused_mut)] to silence clippy
- **MUST** ensure code compiles with no warnings (use `-D warnings` flag in CI, not `#![deny(warnings)]` in source)
- Use `cargo` for building, testing, and dependency management
- Use `cargo test` for running tests
- Use `cargo doc` for generating documentation
- **NEVER** build with `cargo build --features python`: this will always fail. Instead, **ALWAYS** use `maturin`.

## Before Committing

- [ ] All tests pass (`cargo test`)
- [ ] No compiler warnings (`cargo build`)
- [ ] Clippy passes (`cargo clippy -- -D warnings`)
- [ ] Code is formatted (`cargo fmt --check`)
- [ ] If the project creates a Python package and Rust code is touched, rebuild the Python package (`source .venv/bin/activate && maturin develop --release --features python`)
- [ ] If the project creates a WASM package and Rust code is touched, rebuild the WASM package (`wasm-pack build --target web --out-dir web/pkg`)
- [ ] All public items have doc comments
- [ ] No commented-out code or debug statements
- [ ] No hardcoded credentials

---

**Remember:** Prioritize clarity and maintainability over cleverness.
